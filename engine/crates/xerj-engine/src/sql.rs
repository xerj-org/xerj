//! Minimal SQL → Query DSL translator.
//!
//! Supports:
//! ```sql
//! -- Basic query
//! SELECT field1, field2 FROM index WHERE cond AND cond ORDER BY field [ASC|DESC] LIMIT n
//!
//! -- Distinct (de-duplicate by all returned fields)
//! SELECT DISTINCT field1, field2 FROM index WHERE cond
//!
//! -- GROUP BY with COUNT / COUNT(DISTINCT field)
//! SELECT field, COUNT(*) FROM index GROUP BY field
//! SELECT field, COUNT(DISTINCT other_field) FROM index GROUP BY field
//! ```
//!
//! `GROUP BY` is translated to a `terms` aggregation; `COUNT(DISTINCT field)`
//! becomes a `cardinality` sub-aggregation.  The results are returned as ES
//! aggregation buckets rather than tabular rows.

use xerj_query::ast::QueryNode;
use xerj_query::sort::{SortField, SortOrder};

/// An aggregate function found in the SELECT list.
#[derive(Debug, Clone, PartialEq)]
pub enum AggFunction {
    /// `COUNT(*)`
    CountAll,
    /// `COUNT(DISTINCT field)`
    CountDistinct(String),
    /// `COUNT(field)` — count of non-null values
    CountField(String),
}

/// A parsed SQL query.
pub struct SqlQuery {
    /// Target index name.
    pub index: String,
    /// Fields to return (`["*"]` means all).
    pub fields: Vec<String>,
    /// Translated query node.
    pub query: QueryNode,
    /// LIMIT value (if present).
    pub limit: Option<usize>,
    /// ORDER BY fields.
    pub sort: Vec<SortField>,
    /// `SELECT DISTINCT` was specified — results should be deduplicated.
    pub distinct: bool,
    /// GROUP BY fields.
    pub group_by: Vec<String>,
    /// Aggregate functions referenced in the SELECT list.
    pub agg_functions: Vec<AggFunction>,
}

// ── Tokeniser ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Num(f64),
    Str(String),
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    LParen,
    RParen,
    Comma,
    Star,
}

fn tokenise(sql: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => { i += 1; }
            ',' => { tokens.push(Token::Comma); i += 1; }
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            '*' => { tokens.push(Token::Star); i += 1; }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Ge); i += 2;
                } else {
                    tokens.push(Token::Gt); i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Le); i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token::Ne); i += 2;
                } else {
                    tokens.push(Token::Lt); i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Ne); i += 2;
                } else {
                    i += 1;
                }
            }
            '=' => { tokens.push(Token::Eq); i += 1; }
            '\'' | '"' => {
                let quote = chars[i];
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        s.push(chars[i]);
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() { i += 1; } // consume closing quote
                tokens.push(Token::Str(s));
            }
            c if c.is_ascii_digit() || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) => {
                let start = i;
                if chars[i] == '-' { i += 1; }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().collect();
                let n: f64 = raw.parse().unwrap_or(0.0);
                tokens.push(Token::Num(n));
            }
            c if c.is_alphabetic() || c == '_' || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                tokens.push(Token::Word(word));
            }
            _ => { i += 1; }
        }
    }
    tokens
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a SQL string.
///
/// Returns `(index, query, fields, limit, sort)`.
pub fn parse_sql(sql: &str) -> Result<SqlQuery, String> {
    let tokens = tokenise(sql);
    let mut pos = 0usize;

    // Helper closures.
    let peek = |pos: usize| -> Option<&Token> { tokens.get(pos) };
    let peek_word = |pos: usize| -> Option<String> {
        match tokens.get(pos) {
            Some(Token::Word(w)) => Some(w.to_uppercase()),
            _ => None,
        }
    };

    // SELECT
    match peek_word(pos) {
        Some(w) if w == "SELECT" => { pos += 1; }
        _ => return Err("Expected SELECT".to_string()),
    }

    // Optional DISTINCT after SELECT
    let distinct = if peek_word(pos).as_deref() == Some("DISTINCT") {
        pos += 1;
        true
    } else {
        false
    };

    // Fields / aggregate expressions
    let mut fields: Vec<String> = Vec::new();
    let mut agg_functions: Vec<AggFunction> = Vec::new();
    loop {
        match peek(pos) {
            Some(Token::Star) => {
                fields.push("*".to_string());
                pos += 1;
            }
            Some(Token::Word(w)) => {
                let upper = w.to_uppercase();
                if upper == "FROM" || upper == "GROUP" || upper == "ORDER" || upper == "LIMIT" || upper == "WHERE" {
                    break;
                }
                if upper == "COUNT" {
                    pos += 1;
                    // Consume `(`
                    if let Some(Token::LParen) = peek(pos) {
                        pos += 1;
                    } else {
                        return Err("Expected '(' after COUNT".to_string());
                    }
                    // Check for DISTINCT
                    if peek_word(pos).as_deref() == Some("DISTINCT") {
                        pos += 1;
                        // Field name or *
                        match peek(pos) {
                            Some(Token::Word(f)) if f.to_uppercase() != "FROM" => {
                                let field_name = f.clone();
                                pos += 1;
                                agg_functions.push(AggFunction::CountDistinct(field_name));
                            }
                            Some(Token::Star) => {
                                pos += 1;
                                agg_functions.push(AggFunction::CountAll);
                            }
                            _ => return Err("Expected field name after COUNT(DISTINCT".to_string()),
                        }
                    } else {
                        match peek(pos) {
                            Some(Token::Star) => {
                                pos += 1;
                                agg_functions.push(AggFunction::CountAll);
                            }
                            Some(Token::Word(f)) => {
                                let field_name = f.clone();
                                pos += 1;
                                agg_functions.push(AggFunction::CountField(field_name));
                            }
                            _ => return Err("Expected field or * after COUNT(".to_string()),
                        }
                    }
                    // Consume `)`
                    if let Some(Token::RParen) = peek(pos) {
                        pos += 1;
                    } else {
                        return Err("Expected ')' to close COUNT(...)".to_string());
                    }
                } else {
                    fields.push(w.clone());
                    pos += 1;
                }
            }
            _ => break,
        }
        // Consume optional comma.
        if let Some(Token::Comma) = peek(pos) { pos += 1; } else { break; }
    }
    if fields.is_empty() && agg_functions.is_empty() {
        return Err("No fields in SELECT".to_string());
    }

    // FROM
    match peek_word(pos) {
        Some(w) if w == "FROM" => { pos += 1; }
        _ => return Err("Expected FROM".to_string()),
    }

    // Index name.
    let index = match peek(pos) {
        Some(Token::Word(w)) => { let s = w.clone(); pos += 1; s }
        _ => return Err("Expected index name after FROM".to_string()),
    };

    // Optional WHERE.
    let query: QueryNode = if peek_word(pos).as_deref() == Some("WHERE") {
        pos += 1;
        parse_or_expr(&tokens, &mut pos)?
    } else {
        QueryNode::MatchAll
    };

    // Optional GROUP BY.
    let mut group_by: Vec<String> = Vec::new();
    if peek_word(pos).as_deref() == Some("GROUP") {
        pos += 1;
        if peek_word(pos).as_deref() == Some("BY") { pos += 1; }
        loop {
            match peek(pos) {
                Some(Token::Word(field)) => {
                    let upper = field.to_uppercase();
                    if upper == "ORDER" || upper == "LIMIT" || upper == "HAVING" {
                        break;
                    }
                    group_by.push(field.clone());
                    pos += 1;
                }
                _ => break,
            }
            if let Some(Token::Comma) = peek(pos) { pos += 1; } else { break; }
        }
    }

    // Optional HAVING (parsed but not yet translated — reserved for future use).
    if peek_word(pos).as_deref() == Some("HAVING") {
        pos += 1;
        // Skip tokens until ORDER, LIMIT, or end.
        while let Some(tok) = peek(pos) {
            match peek_word(pos).as_deref() {
                Some("ORDER") | Some("LIMIT") => break,
                _ => {}
            }
            match tok {
                Token::Word(_) | Token::Num(_) | Token::Str(_)
                | Token::Eq | Token::Ne | Token::Gt | Token::Ge
                | Token::Lt | Token::Le | Token::LParen | Token::RParen
                | Token::Comma | Token::Star => { pos += 1; }
            }
        }
    }

    // Optional ORDER BY.
    let mut sort: Vec<SortField> = Vec::new();
    if peek_word(pos).as_deref() == Some("ORDER") {
        pos += 1;
        if peek_word(pos).as_deref() == Some("BY") { pos += 1; }
        loop {
            match peek(pos) {
                Some(Token::Word(field)) => {
                    let field = field.clone();
                    pos += 1;
                    let order = match peek_word(pos).as_deref() {
                        Some("DESC") => { pos += 1; SortOrder::Desc }
                        Some("ASC")  => { pos += 1; SortOrder::Asc  }
                        _ => SortOrder::Asc,
                    };
                    sort.push(SortField {
                        field,
                        order,
                        mode: xerj_query::sort::SortMode::default(),
                        missing: xerj_query::sort::SortMissing::default(),
                        format: None,
                    });
                }
                _ => break,
            }
            if let Some(Token::Comma) = peek(pos) { pos += 1; } else { break; }
        }
    }

    // Optional LIMIT.
    let mut limit: Option<usize> = None;
    if peek_word(pos).as_deref() == Some("LIMIT") {
        pos += 1;
        match peek(pos) {
            Some(Token::Num(n)) => { limit = Some(*n as usize); pos += 1; }
            _ => return Err("Expected number after LIMIT".to_string()),
        }
    }

    Ok(SqlQuery { index, fields, query, limit, sort, distinct, group_by, agg_functions })
}

// ── Condition parser (recursive descent) ─────────────────────────────────────

fn parse_or_expr(tokens: &[Token], pos: &mut usize) -> Result<QueryNode, String> {
    let left = parse_and_expr(tokens, pos)?;
    let mut clauses = vec![left];

    while let Some(w) = peek_word_at(tokens, *pos) {
        if w == "OR" {
            *pos += 1;
            clauses.push(parse_and_expr(tokens, pos)?);
        } else {
            break;
        }
    }

    if clauses.len() == 1 {
        Ok(clauses.remove(0))
    } else {
        Ok(QueryNode::Bool {
            must: vec![],
            filter: vec![],
            should: clauses,
            must_not: vec![],
            minimum_should_match: Some(xerj_query::ast::MinShouldMatch::Fixed(1)),
        })
    }
}

fn parse_and_expr(tokens: &[Token], pos: &mut usize) -> Result<QueryNode, String> {
    let left = parse_condition(tokens, pos)?;
    let mut musts = vec![left];

    while let Some(w) = peek_word_at(tokens, *pos) {
        if w == "AND" {
            *pos += 1;
            musts.push(parse_condition(tokens, pos)?);
        } else {
            break;
        }
    }

    if musts.len() == 1 {
        Ok(musts.remove(0))
    } else {
        Ok(QueryNode::Bool {
            must: musts,
            filter: vec![],
            should: vec![],
            must_not: vec![],
            minimum_should_match: None,
        })
    }
}

fn parse_condition(tokens: &[Token], pos: &mut usize) -> Result<QueryNode, String> {
    // Handle NOT / parenthesised groups.
    if let Some(Token::Word(w)) = tokens.get(*pos) {
        if w.to_uppercase() == "NOT" {
            *pos += 1;
            let inner = parse_condition(tokens, pos)?;
            return Ok(QueryNode::Bool {
                must: vec![],
                filter: vec![],
                should: vec![],
                must_not: vec![inner],
                minimum_should_match: None,
            });
        }
    }
    if let Some(Token::LParen) = tokens.get(*pos) {
        *pos += 1;
        let inner = parse_or_expr(tokens, pos)?;
        if let Some(Token::RParen) = tokens.get(*pos) { *pos += 1; }
        return Ok(inner);
    }

    // field op value
    let field = match tokens.get(*pos) {
        Some(Token::Word(f)) => { let f = f.clone(); *pos += 1; f }
        _ => return Err(format!("Expected field name at pos {}", pos)),
    };

    let op = match tokens.get(*pos) {
        Some(Token::Eq)  => { *pos += 1; "eq"   }
        Some(Token::Ne)  => { *pos += 1; "ne"   }
        Some(Token::Gt)  => { *pos += 1; "gt"   }
        Some(Token::Ge)  => { *pos += 1; "gte"  }
        Some(Token::Lt)  => { *pos += 1; "lt"   }
        Some(Token::Le)  => { *pos += 1; "lte"  }
        Some(Token::Word(w)) if w.to_uppercase() == "LIKE"     => { *pos += 1; "like"     }
        Some(Token::Word(w)) if w.to_uppercase() == "NOT"      => {
            // NOT LIKE
            *pos += 1;
            if let Some(Token::Word(w2)) = tokens.get(*pos) {
                if w2.to_uppercase() == "LIKE" { *pos += 1; "not_like" } else { "ne" }
            } else { "ne" }
        }
        _ => return Err(format!("Expected operator at pos {}", pos)),
    };

    let value = match tokens.get(*pos) {
        Some(Token::Str(s))  => { let v = serde_json::Value::String(s.clone()); *pos += 1; v }
        Some(Token::Num(n))  => { let v = serde_json::json!(*n); *pos += 1; v }
        Some(Token::Word(w)) => {
            let upper = w.to_uppercase();
            let v = if upper == "TRUE" {
                serde_json::Value::Bool(true)
            } else if upper == "FALSE" {
                serde_json::Value::Bool(false)
            } else if upper == "NULL" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(w.clone())
            };
            *pos += 1; v
        }
        _ => return Err(format!("Expected value at pos {}", pos)),
    };

    let node = match op {
        "eq" => {
            if let serde_json::Value::String(s) = &value {
                // Use match query for text fields.
                QueryNode::Match {
                    field: field.clone(),
                    query: s.clone(),
                    operator: xerj_query::ast::BoolOperator::default(),
                    boost: None,
                    minimum_should_match: None,
                    analyzer: None,
                }
            } else {
                QueryNode::Term { field: field.clone(), value: value.clone(), boost: None }
            }
        }
        "ne" => QueryNode::Bool {
            must: vec![],
            filter: vec![],
            should: vec![],
            must_not: vec![QueryNode::Term { field: field.clone(), value: value.clone(), boost: None }],
            minimum_should_match: None,
        },
        "gt"  => make_range(&field, None, None, Some(value), None),
        "gte" => make_range(&field, None, None, None, Some(value)),
        "lt"  => make_range(&field, Some(value), None, None, None),
        "lte" => make_range(&field, None, Some(value), None, None),
        "like" => {
            // Convert SQL LIKE pattern (% → *, _ → ?) to wildcard query.
            let pattern = value.as_str().unwrap_or("*").replace('%', "*").replace('_', "?");
            QueryNode::Wildcard { field: field.clone(), value: pattern, boost: None }
        }
        "not_like" => {
            let pattern = value.as_str().unwrap_or("*").replace('%', "*").replace('_', "?");
            QueryNode::Bool {
                must: vec![],
                filter: vec![],
                should: vec![],
                must_not: vec![QueryNode::Wildcard { field: field.clone(), value: pattern, boost: None }],
                minimum_should_match: None,
            }
        }
        _ => QueryNode::MatchAll,
    };
    Ok(node)
}

fn make_range(
    field: &str,
    lt: Option<serde_json::Value>,
    lte: Option<serde_json::Value>,
    gt: Option<serde_json::Value>,
    gte: Option<serde_json::Value>,
) -> QueryNode {
    QueryNode::Range {
        field: field.to_string(),
        gt,
        gte,
        lt,
        lte,
        boost: None,
    }
}

fn peek_word_at(tokens: &[Token], pos: usize) -> Option<String> {
    match tokens.get(pos) {
        Some(Token::Word(w)) => Some(w.to_uppercase()),
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_select() {
        let q = parse_sql("SELECT name, price FROM products WHERE price > 30 LIMIT 3").unwrap();
        assert_eq!(q.index, "products");
        assert_eq!(q.fields, vec!["name", "price"]);
        assert_eq!(q.limit, Some(3));
    }

    #[test]
    fn test_select_star() {
        let q = parse_sql("SELECT * FROM logs").unwrap();
        assert_eq!(q.fields, vec!["*"]);
        assert_eq!(q.index, "logs");
        assert!(matches!(q.query, QueryNode::MatchAll));
    }

    #[test]
    fn test_order_by() {
        let q = parse_sql("SELECT id FROM events ORDER BY created DESC LIMIT 10").unwrap();
        assert_eq!(q.sort.len(), 1);
        assert_eq!(q.sort[0].field, "created");
        assert!(matches!(q.sort[0].order, SortOrder::Desc));
    }
}
