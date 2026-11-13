//! A node budget for YAML parsed from untrusted files.
//!
//! `serde_yaml::Value` materialises every alias as a full deep copy, and
//! the only guards serde_yaml 0.9.34 ships are nesting depth 128 and a
//! jump count of 100 times the event count. The jump guard bounds alias
//! jumps, not expanded nodes: each jump replays a whole anchored subtree,
//! so a 7,351 byte alias bomb expands to 17.85 GiB RSS (#3157). Count the
//! expanded nodes with a visitor that aborts past a budget, before any
//! `Value` is built, and treat an over-budget stream the way each call
//! site already treats a parse error.

use serde::de::{DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// Maximum expanded YAML nodes a single stream may materialise. An 8 KiB
/// sniff prefix holds at most about 4,000 nodes (a node needs at least
/// about 2 bytes of input), so this is 25 times headroom there; the
/// largest YAML the extractor reads (16 MiB) would need at least 200 KB
/// of maximally dense YAML to reach it. A `Value` node costs roughly 50
/// to 150 bytes, so the bound caps transient memory at roughly 10 to
/// 20 MB per file instead of the gigabytes an alias bomb reaches.
pub const YAML_NODE_BUDGET: usize = 100_000;

/// Outcome of counting the expanded nodes of a YAML stream without
/// materialising it.
#[derive(Debug, PartialEq, Eq)]
pub enum YamlBudgetOutcome {
    /// Every document parsed and the total expanded node count fits the
    /// budget. Carries the count.
    WithinBudget(usize),
    /// The expanded node count passed the budget. The stream must be
    /// treated as hostile; callers use their existing parse-error path.
    OverBudget,
    /// The stream does not parse as YAML. Callers keep their existing
    /// error path, which may still extract the documents before the bad
    /// one.
    Invalid,
}

/// Count the expanded nodes of every document in `text`, aborting as
/// soon as the count passes `budget`. Aliases count as the nodes they
/// expand to, because `serde_yaml` replays the anchored events through
/// the visitor on every reference. A parse error anywhere reports
/// `Invalid` rather than failing the whole stream, so callers that
/// extract the good documents before a bad one keep doing so.
///
/// A stream containing an over-budget document is rejected whole, good
/// documents included: a bomb anywhere in the file marks the file
/// hostile, and the loss is recorded as junk by the caller, not hidden.
/// Per-document gating would need re-parsing from the start of the
/// stream for each document, which is itself a CPU blow-up on
/// many-document streams.
pub fn check_yaml_node_budget(text: &str, budget: usize) -> YamlBudgetOutcome {
    use serde::Deserializer as _;
    let state = Rc::new(BudgetState {
        count: Cell::new(0),
        budget,
        exceeded: Cell::new(false),
    });
    for de in serde_yaml::Deserializer::from_str(text) {
        let visitor = CountingVisitor {
            state: Rc::clone(&state),
        };
        if de.deserialize_any(visitor).is_err() {
            if state.exceeded.get() {
                return YamlBudgetOutcome::OverBudget;
            }
            return YamlBudgetOutcome::Invalid;
        }
    }
    YamlBudgetOutcome::WithinBudget(state.count.get())
}

struct BudgetState {
    count: Cell<usize>,
    budget: usize,
    exceeded: Cell<bool>,
}

impl BudgetState {
    /// Count one more expanded node. Past the budget, flag the stream
    /// and fail the visit so the count stops there.
    fn bump<E>(&self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        let next = self.count.get() + 1;
        self.count.set(next);
        if next > self.budget {
            self.exceeded.set(true);
            return Err(E::custom("yaml node budget exceeded"));
        }
        Ok(())
    }
}

struct CountingVisitor {
    state: Rc<BudgetState>,
}

impl<'de> Visitor<'de> for CountingVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any YAML value")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_i64<E>(self, _value: i64) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_u64<E>(self, _value: u64) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_i128<E>(self, _value: i128) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_u128<E>(self, _value: u128) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_f64<E>(self, _value: f64) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_str<E>(self, _value: &str) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_string<E>(self, _value: String) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_bytes<E>(self, _value: &[u8]) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_byte_buf<E>(self, _value: Vec<u8>) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_unit<E>(self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_none<E>(self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.state.bump()
    }

    fn visit_seq<A>(self, mut access: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.state.bump()?;
        while access
            .next_element_seed(CountSeed {
                state: Rc::clone(&self.state),
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut access: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        self.state.bump()?;
        while access
            .next_key_seed(CountSeed {
                state: Rc::clone(&self.state),
            })?
            .is_some()
        {
            access.next_value_seed(CountSeed {
                state: Rc::clone(&self.state),
            })?;
        }
        Ok(())
    }

    fn visit_enum<A>(self, access: A) -> Result<(), A::Error>
    where
        A: EnumAccess<'de>,
    {
        self.state.bump()?;
        let (_tag, variant) = access.variant_seed(CountSeed {
            state: Rc::clone(&self.state),
        })?;
        variant.newtype_variant_seed(CountSeed { state: self.state })?;
        Ok(())
    }
}

/// Drives the counting visitor for a nested value: a sequence element, a
/// mapping key or value, or a tagged value's payload.
struct CountSeed {
    state: Rc<BudgetState>,
}

impl<'de> DeserializeSeed<'de> for CountSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(CountingVisitor { state: self.state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_a_small_mapping() {
        // A mapping (1) with one key (1) and one scalar value (1).
        assert_eq!(
            check_yaml_node_budget("a: 1\n", YAML_NODE_BUDGET),
            YamlBudgetOutcome::WithinBudget(3)
        );
    }

    #[test]
    fn aliases_count_as_the_nodes_they_expand_to() {
        // One anchor of 10 scalars, referenced 10 times. The top mapping
        // (1) plus the `a0` key (1), the anchor sequence (1) and its 10
        // scalars, plus the `a1` key (1), its sequence (1) and 10 expanded
        // aliases of 11 nodes each: 125 in total. A naive event count
        // would say 25.
        let text = "a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n\
                    a1: &a1 [*a0, *a0, *a0, *a0, *a0, *a0, *a0, *a0, *a0, *a0]\n";
        assert_eq!(
            check_yaml_node_budget(text, YAML_NODE_BUDGET),
            YamlBudgetOutcome::WithinBudget(125)
        );
    }

    #[test]
    fn tagged_values_count_through_the_tag() {
        // A mapping (1), the key (1), the tagged node (1), its tag (1)
        // and the scalar payload (1).
        assert_eq!(
            check_yaml_node_budget("a: !foo bar\n", YAML_NODE_BUDGET),
            YamlBudgetOutcome::WithinBudget(5)
        );
    }

    #[test]
    fn budget_boundary_is_exact() {
        // A flow sequence of 4 scalars is 5 nodes: at a budget of 5 it
        // fits, one more scalar trips it.
        assert_eq!(
            check_yaml_node_budget("[1, 1, 1, 1]", 5),
            YamlBudgetOutcome::WithinBudget(5)
        );
        assert_eq!(
            check_yaml_node_budget("[1, 1, 1, 1, 1]", 5),
            YamlBudgetOutcome::OverBudget
        );
    }

    #[test]
    fn unparseable_yaml_reports_invalid() {
        assert_eq!(
            check_yaml_node_budget(
                "# Launch checklist\n- [ ] alpha\n- [ ] beta\n",
                YAML_NODE_BUDGET
            ),
            YamlBudgetOutcome::Invalid
        );
    }

    #[test]
    fn alias_bomb_is_over_budget() {
        let mut s = String::from("---\nt: t\n---\n");
        for _ in 0..12 {
            s.push_str("# c\n");
        }
        s.push_str("a0: &a0 [");
        s.push_str(&vec!["lol"; 1000].join(","));
        s.push_str("]\n");
        for i in 1..=8usize {
            s.push_str(&format!("a{i}: &a{i} ["));
            s.push_str(&vec![format!("*a{}", i - 1); 100].join(","));
            s.push_str("]\n");
        }
        assert_eq!(s.len(), 7351, "fixture must match the reported repro size");
        assert_eq!(
            check_yaml_node_budget(&s, YAML_NODE_BUDGET),
            YamlBudgetOutcome::OverBudget
        );
    }
}
