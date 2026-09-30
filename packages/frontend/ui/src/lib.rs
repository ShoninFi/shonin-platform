//! The design system the frontends share.
//!
//! `tokens.json` is the design's own export: colours, type, spacing, radii,
//! the one shadow and the border widths. This crate turns it into CSS custom
//! properties, so the app and the marketing site read the same values and a
//! design change is one file.

use std::fmt::Write;

use serde::Deserialize;

/// The design's tokens, exactly as exported.
pub const TOKENS: &str = include_str!("../tokens.json");

#[derive(Deserialize)]
pub struct Tokens {
    pub name: String,
    pub color: Colors,
    #[serde(rename = "type")]
    pub typography: Typography,
    pub spacing: Group,
    pub radius: Group,
    pub shadow: Group,
    #[serde(rename = "borderWidth")]
    pub border_width: Group,
}

#[derive(Deserialize)]
pub struct Colors {
    pub tokens: Vec<Token>,
}

#[derive(Deserialize)]
pub struct Group {
    pub tokens: Vec<Token>,
}

#[derive(Deserialize)]
pub struct Token {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub usage: String,
}

#[derive(Deserialize)]
pub struct Typography {
    /// Family name (display, serif, sans, mono) to its CSS font stack.
    pub families: std::collections::BTreeMap<String, String>,
    pub groups: Vec<TypeGroup>,
}

#[derive(Deserialize)]
pub struct TypeGroup {
    pub name: String,
    pub family: String,
    pub styles: Vec<TypeStyle>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeStyle {
    pub name: String,
    pub font_size: String,
    pub line_height: String,
    pub font_weight: serde_json::Value,
    #[serde(default)]
    pub letter_spacing: Option<String>,
}

impl Tokens {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Every token as a custom property on `:root`.
    pub fn css(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "/* Generated from packages/frontend/ui/tokens.json by `task frontend:tokens`.\n   Edit the tokens, not this file. */"
        );
        out.push_str(":root {\n");

        out.push_str("  /* Colour */\n");
        for token in &self.color.tokens {
            property(&mut out, &format!("color-{}", token.name), &token.value);
        }

        out.push_str("\n  /* Type */\n");
        for (family, stack) in &self.typography.families {
            property(&mut out, &format!("font-{family}"), stack);
        }
        for group in &self.typography.groups {
            for style in &group.styles {
                let name = format!("text-{}", style.name);
                property(
                    &mut out,
                    &format!("{name}-family"),
                    &format!("var(--font-{})", group.family),
                );
                property(&mut out, &format!("{name}-size"), &style.font_size);
                property(&mut out, &format!("{name}-line"), &style.line_height);
                let weight = match &style.font_weight {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                property(&mut out, &format!("{name}-weight"), &weight);
                property(
                    &mut out,
                    &format!("{name}-tracking"),
                    style.letter_spacing.as_deref().unwrap_or("0"),
                );
            }
        }

        for (title, group) in [
            ("Spacing", &self.spacing),
            ("Radius", &self.radius),
            ("Shadow", &self.shadow),
            ("Border width", &self.border_width),
        ] {
            let _ = write!(out, "\n  /* {title} */\n");
            for token in &group.tokens {
                property(&mut out, &token.name, &token.value);
            }
        }
        out.push_str("}\n");
        out
    }
}

fn property(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "  --{name}: {value};");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_design_tokens_parse_and_render() {
        let tokens = Tokens::parse(TOKENS).unwrap();
        assert_eq!(tokens.name, "Shonin");
        let css = tokens.css();
        assert!(css.starts_with("/* Generated"));
        assert!(css.contains("  --color-paper: #f6f7fb;\n"));
        assert!(css.contains("  --font-display: "));
        assert!(css.contains("  --text-display-xl-size: 64px;\n"));
        assert!(css.contains("  --text-display-xl-weight: 800;\n"));
        assert!(css.contains("  --space-1: 4px;\n"));
        assert!(css.contains("  --radius-m: 10px;\n"));
        assert!(css.contains("  --shadow-float: "));
        assert!(css.contains("  --border-hairline: 1px;\n"));
        assert!(css.trim_end().ends_with('}'));
    }
}
