//! The design tokens as CSS custom properties, named as the design names them.
//!
//! The values come from the `ui` crate's `tokens.json`, the design system's
//! own export, so a design change is still one file. The names are the
//! design's: `--paper`, `--brand`, `--space-4`, `--radius-m`. An alias such as
//! `{brand}` becomes `var(--brand)`, never a copy of the hex.
//!
//! Each type style also becomes a `font` shorthand, `--type-<name>`, and its
//! tracking, `--track-<name>`, so a rule reads `font: var(--type-display-l)`.

use std::fmt::Write;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// `:root { ... }` with every token in `tokens.json`.
pub fn css() -> Result<String> {
    let json: Value = serde_json::from_str(ui::TOKENS).context("parsing tokens.json")?;
    let mut out = String::from(
        "/* The design tokens, generated from tokens.json by the marketing build. */\n:root {\n",
    );

    for token in list(&json, &["color", "tokens"])? {
        let (name, value) = pair(token)?;
        property(&mut out, name, &alias(value));
    }

    let families = json
        .pointer("/type/families")
        .and_then(Value::as_object)
        .context("tokens.json has no type families")?;
    for (family, stack) in families {
        let stack = stack.as_str().context("a font stack is not text")?;
        property(&mut out, &format!("font-{family}"), stack);
    }
    for group in list(&json, &["type", "groups"])? {
        let family = text(group, "family")?;
        for style in group
            .get("styles")
            .and_then(Value::as_array)
            .context("a type group has no styles")?
        {
            let name = text(style, "name")?;
            let weight = match style.get("fontWeight") {
                Some(Value::Number(number)) => number.to_string(),
                Some(Value::String(weight)) => weight.clone(),
                _ => bail!("the type style {name} has no weight"),
            };
            let size = text(style, "fontSize")?;
            let line = text(style, "lineHeight")?;
            property(
                &mut out,
                &format!("type-{name}"),
                &format!("{weight} {size}/{line} var(--font-{family})"),
            );
            let tracking = style
                .get("letterSpacing")
                .and_then(Value::as_str)
                .unwrap_or("0");
            property(&mut out, &format!("track-{name}"), tracking);
        }
    }

    for group in ["spacing", "radius", "shadow", "borderWidth"] {
        for token in list(&json, &[group, "tokens"])? {
            let (name, value) = pair(token)?;
            property(&mut out, name, &alias(value));
        }
    }
    out.push_str("}\n");
    Ok(out)
}

fn list<'a>(json: &'a Value, path: &[&str]) -> Result<&'a Vec<Value>> {
    let mut at = json;
    for key in path {
        at = at
            .get(key)
            .with_context(|| format!("tokens.json has no {}", path.join(".")))?;
    }
    at.as_array()
        .with_context(|| format!("{} is not a list", path.join(".")))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("a token has no {key}"))
}

fn pair(token: &Value) -> Result<(&str, &str)> {
    Ok((text(token, "name")?, text(token, "value")?))
}

/// `{brand}` is the brand token itself, not its value.
fn alias(value: &str) -> String {
    match value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        Some(name) => format!("var(--{name})"),
        None => value.to_string(),
    }
}

fn property(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "  --{name}: {value};");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_render_under_the_design_s_own_names() {
        let css = css().unwrap();
        assert!(css.contains("  --paper: #f6f7fb;\n"));
        assert!(css.contains("  --link: var(--brand);\n"));
        assert!(css.contains("  --type-display-l: 800 44px/50px var(--font-display);\n"));
        assert!(css.contains("  --track-label: 0.06em;\n"));
        assert!(css.contains("  --space-8: 80px;\n"));
        assert!(css.contains("  --radius-l: 16px;\n"));
        assert!(css.contains("  --border-control: 1.5px;\n"));
    }
}
