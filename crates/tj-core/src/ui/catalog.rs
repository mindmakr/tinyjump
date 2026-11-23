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
    // A number, currency or percentage, formatted by the platform's own locale formatter.
    Kind { name: "Format", props: &["value", "kind", "currency", "decimals", "locale"], events: &[], content: Content::None },
    Kind { name: "Tabs", props: &["items", "value"], events: &["onChange"], content: Content::None },
    Kind { name: "Icon", props: &["name"], events: &[], content: Content::None },
    Kind { name: "List", props: &["itemHeight", "horizontal"], events: &["onEndReached"], content: Content::Nodes },
    Kind { name: "Theme", props: &["mode", "accent", "surface", "muted"], events: &[], content: Content::Nodes },
    // A host-registered native view (Web element, Android View, iOS UIView) with arbitrary props and `on<Upper>...` events.
    Kind { name: "Native", props: &["name"], events: &[], content: Content::Nodes },
];

pub fn kind(name: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.name == name)
}

/// Whether `name` is a valid custom native component name (e.g. `"MapView"`):
/// starts with an ASCII capital letter, uses ASCII letters/digits/`_`, and does not shadow a built-in.
pub fn valid_native_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && kind(name).is_none()
}

/// Whether `prop` follows the `on<Upper>...` event handler convention on `<Native>`.
pub fn is_native_event(prop: &str) -> bool {
    prop.len() > 2 && prop.starts_with("on") && prop.as_bytes()[2].is_ascii_uppercase()
}

impl Kind {
    pub fn allows(&self, prop: &str) -> bool {
        self.name == "Native" || COMMON_PROPS.contains(&prop) || self.props.contains(&prop) || self.events.contains(&prop)
    }

    pub fn is_event(&self, prop: &str) -> bool {
        self.events.contains(&prop) || (self.name == "Native" && is_native_event(prop))
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
