use rustc_hash::{FxHashMap, FxHashSet};

use biome_analyze::{Rule, RuleDiagnostic, RuleSource, context::RuleContext, declare_lint_rule};
use biome_console::markup;
use biome_css_semantic::model::{Rule as CssSemanticRule, RuleId, SemanticModel, Specificity};
use biome_css_syntax::{AnyCssSelector, CssRoot};
use biome_diagnostics::Severity;
use biome_rowan::TextRange;

use biome_rowan::AstNode;

use crate::services::semantic::Semantic;

declare_lint_rule! {
    /// Disallow a lower specificity selector from coming after a higher specificity selector.
    ///
    /// Source order is important in CSS, and when two selectors have the same specificity, the one that occurs last will take priority.
    /// However, the situation is different when one of the selectors has a higher specificity.
    /// In that case, source order does not matter: the selector with higher specificity will win out even if it comes first.
    ///
    /// The clashes of these two mechanisms for prioritization, source order and specificity, can cause some confusion when reading stylesheets.
    /// If a selector with higher specificity comes before the selector it overrides, we have to think harder to understand it, because it violates the source order expectation.
    /// **Stylesheets are most legible when overriding selectors always come after the selectors they override.**
    /// That way both mechanisms, source order and specificity, work together nicely.
    ///
    /// This rule enforces that practice as best it can, reporting fewer errors than it should.
    /// It cannot catch every actual overriding selector, but it can catch certain common mistakes.
    ///
    /// ## Examples
    ///
    /// ### Invalid
    ///
    /// ```css,expect_diagnostic
    /// b a { color: red; }
    /// a { color: red; }
    /// ```
    ///
    /// ```css,expect_diagnostic
    /// a {
    ///   & > b { color: red; }
    /// }
    /// b { color: red; }
    /// ```
    ///
    /// ```css,expect_diagnostic
    /// :root input {
    ///     color: red;
    /// }
    /// html input {
    ///     color: red;
    /// }
    /// ```
    ///
    ///
    /// ### Valid
    ///
    /// ```css
    /// a { color: red; }
    /// b a { color: red; }
    /// ```
    ///
    /// ```css
    /// b { color: red; }
    /// a {
    ///   & > b { color: red; }
    /// }
    /// ```
    ///
    /// ```css
    /// a:hover { color: red; }
    /// a { color: red; }
    /// ```
    ///
    /// ```css
    /// a b {
    ///     color: red;
    /// }
    /// /* This selector is overwritten by the one above it, but this is not an error because the rule only evaluates it as a compound selector */
    /// :where(a) :is(b) {
    ///     color: blue;
    /// }
    /// ```
    ///
    pub NoDescendingSpecificity {
        version: "1.9.3",
        name: "noDescendingSpecificity",
        language: "css",
        recommended: true,
        severity: Severity::Warning,
        sources: &[RuleSource::Stylelint("no-descending-specificity").same()],
    }
}

#[derive(Debug)]
pub struct DescendingSelector {
    high: (TextRange, Specificity),
    low: (TextRange, Specificity),
}
/// find tail selector
/// ```css
/// a b:hover {
///   ^^^^^^^
/// }
/// ```
fn find_tail_selector_str(selector: &AnyCssSelector) -> Option<String> {
    match selector {
        AnyCssSelector::CssCompoundSelector(s) => {
            let simple = s
                .simple_selector()
                .map_or(String::new(), |s| s.syntax().text_trimmed().to_string());
            let sub = s.sub_selectors().syntax().text_trimmed().to_string();

            let last_selector = [simple, sub].join("");
            Some(last_selector)
        }
        AnyCssSelector::CssComplexSelector(s) => {
            s.right().as_ref().ok().and_then(find_tail_selector_str)
        }
        _ => None,
    }
}

/// This function traverses the CSS rules starting from the given rule and checks for selectors that have the same tail selector.
/// For each selector, it compares its specificity with the previously encountered specificity of the same tail selector.
/// If a lower specificity selector is found after a higher specificity selector with the same tail selector, it records this as a descending selector.
fn find_descending_selector(
    rule: &CssSemanticRule,
    model: &SemanticModel,
    visited_rules: &mut FxHashSet<RuleId>,
    visited_selectors: &mut FxHashMap<String, (TextRange, Specificity)>,
    descending_selectors: &mut Vec<DescendingSelector>,
) {
    if !visited_rules.insert(rule.id()) {
        return;
    }

    for selector in rule.selectors() {
        let Some(casted_selector) = AnyCssSelector::cast(selector.node().clone()) else {
            continue;
        };
        let Some(tail_selector_str) = find_tail_selector_str(&casted_selector) else {
            continue;
        };

        if let Some((last_text_range, last_specificity)) = visited_selectors.get(&tail_selector_str)
        {
            if last_specificity > &selector.specificity() {
                descending_selectors.push(DescendingSelector {
                    high: (*last_text_range, *last_specificity),
                    low: (selector.range(), selector.specificity()),
                });
            }
        } else {
            visited_selectors.insert(
                tail_selector_str,
                (selector.range(), selector.specificity()),
            );
        }
    }

    for child_id in rule.child_ids() {
        if let Some(child_rule) = model.get_rule_by_id(*child_id) {
            find_descending_selector(
                child_rule,
                model,
                visited_rules,
                visited_selectors,
                descending_selectors,
            );
        }
    }
}

impl Rule for NoDescendingSpecificity {
    type Query = Semantic<CssRoot>;
    type State = DescendingSelector;
    type Signals = Box<[Self::State]>;
    type Options = ();

    fn run(ctx: &RuleContext<Self>) -> Self::Signals {
        let model = ctx.model();
        let mut visited_rules = FxHashSet::default();
        let mut visited_selectors = FxHashMap::default();
        let mut descending_selectors = Vec::new();
        for rule in model.rules() {
            find_descending_selector(
                rule,
                model,
                &mut visited_rules,
                &mut visited_selectors,
                &mut descending_selectors,
            );
        }
        descending_selectors.into_boxed_slice()
    }

    fn diagnostic(_: &RuleContext<Self>, node: &Self::State) -> Option<RuleDiagnostic> {
        Some(
            RuleDiagnostic::new(
                rule_category!(),
                node.low.0,
                markup! {
                    "Descending specificity selector found. This selector specificity is "{node.low.1.to_string()}
                },
            ).detail(node.high.0, markup!(
                "This selector specificity is "{node.high.1.to_string()}
            ))
            .note(markup! {
                    "Descending specificity selector may not applied. Consider rearranging the order of the selectors. See "<Hyperlink href="https://developer.mozilla.org/en-US/docs/Web/CSS/Specificity">"MDN web docs"</Hyperlink>" for more details."
            }),
        )
    }
}
