//! A node budget for YAML parsed from untrusted files.
//!
//! `serde_yaml::Value` materialises every alias as a full deep copy, and
//! the only guards serde_yaml 0.9.34 ships are nesting depth 128 and a
//! jump count of 100 times the event count. The jump guard bounds alias
//! jumps, not expanded nodes: each jump replays a whole anchored subtree,
//! so a 7,351 byte alias bomb expands to 17.85 GiB RSS (#3157). Count the
//! expanded nodes with a visitor that aborts past a budget, before any
//! `Value` is built, and treat an over-budget stream the way each call
//! site already treats a parse error. The budget scales with the input
//! size (see [`yaml_node_budget`]): a large ordinary file gets headroom,
//! a small input that expands hugely does not.

use serde::de::{DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// Floor for the node budget: the smallest budget any stream gets, so a
/// small input that expands hugely is still rejected at the same place.
/// A 7,351 byte alias bomb expands past this long before the counting
/// visitor has done any real work, and the count aborts before any
/// `Value` is built.
pub const YAML_NODE_BUDGET: usize = 100_000;

/// One expanded node per this many input bytes. Ordinary YAML carries at
/// most about one node per 2 bytes (a node needs at least a key or scalar
/// plus its punctuation, and the record-style files the extractor sees
/// run closer to one node per 5 bytes), so a stream that needs more nodes
/// than half its bytes must be expanding aliases. Kept as a divisor so
/// the budget stays in integer arithmetic.
const YAML_NODE_BUDGET_BYTES_PER_NODE: usize = 2;

/// Budget for one stream of `text`: the floor, or one node per
/// [`YAML_NODE_BUDGET_BYTES_PER_NODE`] input bytes, whichever is larger.
/// A large but ordinary file scales up with its size instead of being
/// junked whole: 20k list items with 4 fields each (about 1 MB, roughly
/// 180k nodes) gets a budget of about 500k, while the 7,351 byte alias
/// bomb still gets the 100,000 floor and is rejected.
///
/// The trade-off is explicit: the largest YAML the extractor reads
/// (16 MiB) now budgets about 8.4M nodes, so a file that genuinely holds
/// that many nodes may transiently materialise roughly 400 MB to 1.3 GB
/// (50 to 150 bytes per `Value` node) instead of the 10 to 20 MB the flat
/// budget capped. What the scaling still stops is the bomb shape: a large
/// expansion from a small input, which is what an alias bomb is.
pub fn yaml_node_budget(text: &str) -> usize {
    YAML_NODE_BUDGET.max(text.len() / YAML_NODE_BUDGET_BYTES_PER_NODE)
}

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
        // A 7,351 byte input budgets the 100,000 floor, so the bomb is
        // still rejected at the same place under the scaled rule.
        assert_eq!(
            check_yaml_node_budget(&s, yaml_node_budget(&s)),
            YamlBudgetOutcome::OverBudget
        );
    }

    #[test]
    fn budget_scales_with_input_size() {
        // Small inputs keep the floor.
        assert_eq!(yaml_node_budget("a: 1\n"), 100_000);
        assert_eq!(yaml_node_budget(&"x".repeat(199_999)), 100_000);
        // Past the floor the budget is one node per two input bytes.
        assert_eq!(yaml_node_budget(&"x".repeat(1_000_000)), 500_000);
        assert_eq!(yaml_node_budget(&"x".repeat(16 << 20)), 8_388_608);
    }

    /// The maintainer's example: 20k list items with 4 fields each, about
    /// 1 MB. One top-level sequence (1) plus 9 nodes per item (a mapping,
    /// 4 keys, 4 scalar values): 180,001 nodes in total.
    fn large_record_yaml() -> String {
        let mut s = String::with_capacity(1_150_000);
        for i in 0..20_000usize {
            s.push_str(&format!(
                "- id: {i}\n  name: item{i}\n  flag: true\n  score: 1.5\n"
            ));
        }
        s
    }

    #[test]
    fn large_ordinary_yaml_fits_the_scaled_budget() {
        let text = large_record_yaml();
        assert!(
            (1_000_000..1_500_000).contains(&text.len()),
            "fixture must be about 1 MB, got {} bytes",
            text.len()
        );
        // The flat 100,000 budget junked this file whole.
        assert_eq!(
            check_yaml_node_budget(&text, YAML_NODE_BUDGET),
            YamlBudgetOutcome::OverBudget
        );
        // The scaled budget (one node per two bytes) lets it through.
        assert_eq!(
            check_yaml_node_budget(&text, yaml_node_budget(&text)),
            YamlBudgetOutcome::WithinBudget(180_001)
        );
    }
}
