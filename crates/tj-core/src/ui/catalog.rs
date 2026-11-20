//! The component catalog: the one list of UI primitives that every host draws natively.
//! The compiler checks JSX against it, and the docs and hosts mirror it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// No children.
    None,
    /// Children are joined into the `text` prop.
    Text,
    /// Children are child nodes; bare strings become `Text` nodes.
    Nodes,
}

#[derive(Debug)]
pub struct Kind {
    pub name: &'static str,
    pub props: &'static [&'static str],
    pub events: &'static [&'static str],
    pub content: Content,
}

/// Props every component accepts: identity, accessibility and layout/style.
pub const COMMON_PROPS: &[&str] = &[
    "id", "key", "label", "padding", "margin", "gap", "align", "justify", "grow", "width", "height", "color", "background", "radius", "size", "weight", "border",
];

pub const KINDS: &[Kind] = &[
    Kind { name: "Column", props: &[], events: &[], content: Content::Nodes },
    Kind { name: "Row", props: &[], events: &[], content: Content::Nodes },
    Kind { name: "Scroll", props: &[], events: &[], content: Content::Nodes },
    Kind { name: "Spacer", props: &[], events: &[], content: Content::None },
    Kind { name: "Text", props: &["lines"], events: &[], content: Content::Text },
    Kind { name: "Button", props: &["disabled"], events: &["onPress"], content: Content::Text },
    Kind { name: "Input", props: &["value", "placeholder", "secure", "multiline", "type", "zone"], events: &["onChange", "onSubmit"], content: Content::None },
    Kind { name: "Toggle", props: &["value"], events: &["onChange"], content: Content::Text },
    Kind { name: "Image", props: &["src", "fit"], events: &[], content: Content::None },
    // A time, shown by the platform's own formatter in the viewer's language and zone.
    Kind { name: "Time", props: &["value", "show", "style", "zone"], events: &[], content: Content::None },
];

pub fn kind(name: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.name == name)
}

impl Kind {
    pub fn allows(&self, prop: &str) -> bool {
        COMMON_PROPS.contains(&prop) || self.props.contains(&prop) || self.events.contains(&prop)
    }

    pub fn is_event(&self, prop: &str) -> bool {
        self.events.contains(&prop)
    }

    /// Every prop name this component accepts, for error hints.
    pub fn all_props(&self) -> Vec<&'static str> {
        self.props.iter().chain(self.events).chain(COMMON_PROPS).copied().collect()
    }
}

/// Component names, for error hints.
pub fn names() -> String {
    KINDS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agents load docs/language.md (`tj docs`, the MCP `language` tool) instead of reading
    /// the code, so it must name every component, prop, event and built-in.
    #[test]
    fn the_language_reference_lists_every_component_and_builtin() {
        let doc = include_str!("../../../../docs/language.md");
        let mentioned = |name: &str| doc.contains(&format!("`{name}`")) || doc.contains(&format!("`{name}(")) || doc.contains(&format!("{name},")) || doc.contains(&format!("{name}`")) || doc.contains(&format!("`{name}."));
        let mut missing: Vec<&str> = Vec::new();
        for k in KINDS {
            missing.extend(std::iter::once(k.name).chain(k.props.iter().copied()).chain(k.events.iter().copied()).filter(|n| !mentioned(n)));
        }
        missing.extend(COMMON_PROPS.iter().copied().filter(|n| !mentioned(n)));
        missing.extend(crate::vm::builtins::GLOBALS.iter().map(|(n, _)| n.trim_start_matches("__")).filter(|n| *n != "sort" && !mentioned(n)));
        assert!(missing.is_empty(), "docs/language.md does not mention: {missing:?}");
    }
}
