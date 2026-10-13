//! The design system the frontends share.
//!
//! `tokens.json` is the design's own export: colours, type, spacing, radii,
//! the one shadow, the border widths and the motion timings. This crate turns
//! it into CSS custom properties named as the design names them (`--paper`,
//! `--brand`, `--space-4`, `--radius-m`), so the app and the marketing site
//! read the same values and a design change is one file.
//!
//! It also holds everything else both sites draw with, so an atom looks the
//! same on both origins:
//!
//! - [`styles`]: one stylesheet per atom and component (`sh-*` classes),
//!   written only in the tokens' custom properties.
//! - [`fonts_css`] and [`FONTS`]: the self-hosted variable fonts.
//! - [`icons`]: the 24px line icon set, as data.
//! - [`logos`]: the logo files, exactly as drawn.
//! - [`images`]: the share images and the PNG icons, drawn from the mark.
//!
//! `task frontend:tokens` writes all of it into `dist/` (see `src/bin/tokens.rs`).

use std::fmt::Write;

use serde::Deserialize;

pub mod icons;
pub mod images;
pub mod logos;
pub mod styles;

/// The design's tokens, exactly as exported (plus what this build adds and
/// sends back to the design system: `display-hero`, the motion group and the
/// colour literals the canvas hard-coded).
pub const TOKENS: &str = include_str!("../tokens.json");

#[derive(Deserialize)]
pub struct Tokens {
    pub name: String,
    pub color: Group,
    #[serde(rename = "type")]
    pub typography: Typography,
    pub spacing: Group,
    pub radius: Group,
    pub shadow: Group,
    #[serde(rename = "borderWidth")]
    pub border_width: Group,
    #[serde(default)]
    pub motion: Option<Group>,
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
    /// Every font file: its family, file, weight range and style.
    pub fonts: Vec<FontFile>,
    /// Family name (display, serif, sans, mono) to its CSS font stack.
    pub families: std::collections::BTreeMap<String, String>,
    pub groups: Vec<TypeGroup>,
}

#[derive(Deserialize)]
pub struct FontFile {
    pub family: String,
    /// `fonts/HankenGrotesk-Variable.woff2`
    pub file: String,
    pub weight: String,
    pub style: String,
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
    #[serde(default)]
    pub font_style: Option<String>,
}

impl TypeStyle {
    fn weight(&self) -> String {
        match &self.font_weight {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }

    /// Figures and every number in tables and stats use tabular numerals.
    fn is_figure(&self) -> bool {
        self.name.starts_with("figure")
    }
}

/// A token value, with the design's `{name}` aliases written as references to
/// the token they name, never a copy of its value.
pub fn alias(value: &str) -> String {
    match value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        Some(name) => format!("var(--{name})"),
        None => value.to_string(),
    }
}

impl Tokens {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Every token as a custom property on `:root`, then one `.t-<style>`
    /// utility class per type style.
    ///
    /// Colours are `--<name>`; families `--font-<family>`; each type style a
    /// `font` shorthand `--type-<name>` and its tracking `--track-<name>`;
    /// spacing, radii, the shadow, border widths and motion by their own
    /// names (`--space-4`, `--radius-m`, `--dur-fast`, `--ease-out`).
    pub fn css(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "/* Generated from packages/frontend/ui/tokens.json by `task frontend:tokens`.\n   Edit the tokens, not this file. Light only: there is no dark theme. */"
        );
        out.push_str(":root {\n");

        out.push_str("  /* Colour */\n");
        for token in &self.color.tokens {
            property(&mut out, &token.name, &alias(&token.value));
        }

        out.push_str("\n  /* Type */\n");
        for (family, stack) in &self.typography.families {
            property(&mut out, &format!("font-{family}"), stack);
        }
        for group in &self.typography.groups {
            for style in &group.styles {
                let italic = if style.font_style.as_deref() == Some("italic") {
                    "italic "
                } else {
                    ""
                };
                property(
                    &mut out,
                    &format!("type-{}", style.name),
                    &format!(
                        "{italic}{} {}/{} var(--font-{})",
                        style.weight(),
                        style.font_size,
                        style.line_height,
                        group.family
                    ),
                );
                property(
                    &mut out,
                    &format!("track-{}", style.name),
                    style.letter_spacing.as_deref().unwrap_or("0"),
                );
            }
        }

        let mut groups = vec![
            ("Spacing", &self.spacing),
            ("Radius", &self.radius),
            ("Shadow", &self.shadow),
            ("Border width", &self.border_width),
        ];
        if let Some(motion) = &self.motion {
            groups.push(("Motion", motion));
        }
        for (title, group) in groups {
            let _ = write!(out, "\n  /* {title} */\n");
            for token in &group.tokens {
                property(&mut out, &token.name, &alias(&token.value));
            }
        }
        out.push_str("}\n\n/* One class per type style. */\n");

        for group in &self.typography.groups {
            for style in &group.styles {
                let name = &style.name;
                let _ = write!(
                    out,
                    ".t-{name} {{ font: var(--type-{name}); letter-spacing: var(--track-{name});"
                );
                if style.is_figure() {
                    out.push_str(" font-variant-numeric: tabular-nums;");
                }
                if group.family == "serif" {
                    let _ = write!(
                        out,
                        " font-variation-settings: \"opsz\" {};",
                        style.font_size.trim_end_matches("px")
                    );
                }
                out.push_str(" }\n");
            }
        }
        out
    }

    /// `@font-face` for every font file, self-hosted at `/fonts/<file>`, with
    /// `font-display: swap` so text shows at once in a fallback face.
    pub fn fonts_css(&self) -> String {
        let mut out = String::from(
            "/* Generated from packages/frontend/ui/tokens.json by `task frontend:tokens`.\n   The design system's variable fonts, self-hosted. All four families are under the\n   SIL Open Font License 1.1 (fonts/README). */\n",
        );
        for font in &self.typography.fonts {
            let file = font.file.rsplit('/').next().unwrap_or(&font.file);
            let _ = writeln!(
                out,
                "@font-face {{ font-family: \"{}\"; font-style: {}; font-weight: {}; font-display: swap; src: url(\"/fonts/{file}\") format(\"woff2\"); }}",
                font.family, font.style, font.weight
            );
        }
        out
    }
}

fn property(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "  --{name}: {value};");
}

/// The font files, by file name, for a build to copy to `/fonts/`.
pub const FONTS: &[(&str, &[u8])] = &[
    (
        "HankenGrotesk-Variable.woff2",
        include_bytes!("../fonts/HankenGrotesk-Variable.woff2"),
    ),
    (
        "HankenGrotesk-Variable-Italic.woff2",
        include_bytes!("../fonts/HankenGrotesk-Variable-Italic.woff2"),
    ),
    (
        "PlusJakartaSans-Variable.woff2",
        include_bytes!("../fonts/PlusJakartaSans-Variable.woff2"),
    ),
    (
        "Newsreader-Variable.woff2",
        include_bytes!("../fonts/Newsreader-Variable.woff2"),
    ),
    (
        "Newsreader-Variable-Italic.woff2",
        include_bytes!("../fonts/Newsreader-Variable-Italic.woff2"),
    ),
    (
        "JetBrainsMono-Variable.woff2",
        include_bytes!("../fonts/JetBrainsMono-Variable.woff2"),
    ),
];

/// The two faces above the fold on every page, worth a `<link rel=preload>`.
/// Newsreader and JetBrains Mono load on first use.
pub const PRELOAD_FONTS: [&str; 2] = [
    "HankenGrotesk-Variable.woff2",
    "PlusJakartaSans-Variable.woff2",
];

/// The font licence notice that ships beside the files.
pub const FONTS_README: &str = include_str!("../fonts/README");

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens::parse(TOKENS).unwrap()
    }

    #[test]
    fn the_design_tokens_render_under_the_design_s_own_names() {
        let tokens = tokens();
        assert_eq!(tokens.name, "Shonin");
        let css = tokens.css();
        assert!(css.starts_with("/* Generated"));
        assert!(css.contains("  --paper: #f6f7fb;\n"));
        assert!(css.contains("  --font-display: "));
        assert!(css.contains("  --type-display-xl: 800 64px/68px var(--font-display);\n"));
        assert!(css.contains("  --track-display-xl: -0.03em;\n"));
        assert!(css.contains("  --space-1: 4px;\n"));
        assert!(css.contains("  --radius-m: 10px;\n"));
        assert!(css.contains("  --shadow-float: "));
        assert!(css.contains("  --border-hairline: 1px;\n"));
        assert!(css.contains(".t-body { font: var(--type-body);"));
    }

    #[test]
    fn an_alias_refers_to_its_token_rather_than_copying_it() {
        let css = tokens().css();
        assert!(css.contains("  --link: var(--brand);\n"));
        assert!(css.contains("  --danger: var(--loss);\n"));
        assert!(!css.contains('{') || !css.contains("{brand}"));
        assert_eq!(alias("#fff"), "#fff");
    }

    #[test]
    fn the_hero_headline_has_its_own_type_style() {
        let css = tokens().css();
        assert!(css.contains("  --type-display-hero: 800 72px/76px var(--font-display);\n"));
        assert!(css.contains("  --track-display-hero: -0.035em;\n"));
    }

    #[test]
    fn the_reviewer_s_words_are_set_in_italic() {
        let css = tokens().css();
        assert!(css.contains("  --type-display-quote: italic 400 20px/30px var(--font-serif);\n"));
    }

    #[test]
    fn figures_use_tabular_numerals() {
        let css = tokens().css();
        let line = css
            .lines()
            .find(|line| line.starts_with(".t-figure-l "))
            .unwrap();
        assert!(line.contains("tabular-nums"));
    }

    #[test]
    fn motion_is_tokens_not_numbers() {
        let css = tokens().css();
        for name in [
            "dur-fast",
            "dur-base",
            "dur-panel",
            "dur-page",
            "dur-reveal",
            "dur-draw",
            "ease-out",
            "ease-emph",
            "ease-in-out",
            "stagger",
        ] {
            assert!(css.contains(&format!("  --{name}: ")), "{name}");
        }
    }

    #[test]
    fn every_font_file_has_a_face_and_ships() {
        let tokens = tokens();
        let css = tokens.fonts_css();
        assert_eq!(
            css.matches("@font-face").count(),
            tokens.typography.fonts.len()
        );
        assert_eq!(css.matches("font-display: swap").count(), FONTS.len());
        for font in &tokens.typography.fonts {
            let file = font.file.rsplit('/').next().unwrap();
            assert!(css.contains(&format!("url(\"/fonts/{file}\")")));
            assert!(
                FONTS
                    .iter()
                    .any(|(name, bytes)| *name == file && !bytes.is_empty())
            );
        }
        for preload in PRELOAD_FONTS {
            assert!(FONTS.iter().any(|(name, _)| *name == preload));
        }
        assert!(FONTS_README.contains("SIL Open Font License"));
    }
}
