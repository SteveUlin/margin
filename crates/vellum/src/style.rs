//! Cell style: colours, attributes, underline style and colour, hyperlink id.
//! SGR emission lives here so the grid emits minimal transitions between two styles.

use crate::color::Rgb;
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub strike: bool,
    pub reverse: bool,
    pub ul: Underline,
    pub ul_color: Option<Rgb>,
    /// Index into the grid's link table; 0 means no hyperlink.
    pub link: u16,
}

impl Style {
    pub fn fg(c: Rgb) -> Style {
        Style { fg: Some(c), ..Default::default() }
    }
    pub fn on(mut self, c: Rgb) -> Style {
        self.bg = Some(c);
        self
    }
    pub fn with_fg(mut self, c: Rgb) -> Style {
        self.fg = Some(c);
        self
    }
    pub fn bold(mut self) -> Style {
        self.bold = true;
        self
    }
    pub fn italic(mut self) -> Style {
        self.italic = true;
        self
    }
    pub fn dim(mut self) -> Style {
        self.dim = true;
        self
    }
    pub fn strike(mut self) -> Style {
        self.strike = true;
        self
    }
    pub fn underline(mut self, u: Underline, c: Option<Rgb>) -> Style {
        self.ul = u;
        self.ul_color = c;
        self
    }
    pub fn bold_if(self, b: bool) -> Style {
        if b {
            self.bold()
        } else {
            self
        }
    }
}

fn ul_code(u: Underline) -> &'static str {
    match u {
        Underline::None => "24",
        Underline::Single => "4",
        Underline::Curly => "4:3",
        Underline::Dotted => "4:4",
        Underline::Dashed => "4:5",
    }
}

/// Append the SGR parameters that move the terminal from style `a` to style `b` (without the
/// `ESC[` / `m` framing). Returns false when nothing needs emitting.
///
/// Turning an attribute off costs a specific code (22, 23, 24, 27, 29, 39, 49, 59); when more
/// attributes turn off than stay on, a full reset (`0`) followed by `b`'s attributes is shorter.
pub fn sgr_transition(a: &Style, b: &Style, out: &mut String) -> bool {
    let (a, b) = (strip_link(a), strip_link(b));
    if a == b {
        return false;
    }
    let offs = (a.bold && !b.bold) as u8
        + (a.dim && !b.dim) as u8
        + (a.italic && !b.italic) as u8
        + (a.strike && !b.strike) as u8
        + (a.reverse && !b.reverse) as u8
        + (a.ul != Underline::None && b.ul == Underline::None) as u8
        + (a.fg.is_some() && b.fg.is_none()) as u8
        + (a.bg.is_some() && b.bg.is_none()) as u8;
    let mut parts: Vec<String> = Vec::with_capacity(8);
    let base = if offs >= 2 {
        parts.push("0".into());
        Style::default()
    } else {
        a
    };
    // bold and dim share their off code (22), so re-assert the survivor after turning one off
    if (base.bold && !b.bold) || (base.dim && !b.dim) {
        parts.push("22".into());
        if b.bold {
            parts.push("1".into());
        }
        if b.dim {
            parts.push("2".into());
        }
    } else {
        if b.bold && !base.bold {
            parts.push("1".into());
        }
        if b.dim && !base.dim {
            parts.push("2".into());
        }
    }
    if b.italic != base.italic {
        parts.push(if b.italic { "3" } else { "23" }.into());
    }
    if b.ul != base.ul {
        parts.push(ul_code(b.ul).into());
    }
    if b.reverse != base.reverse {
        parts.push(if b.reverse { "7" } else { "27" }.into());
    }
    if b.strike != base.strike {
        parts.push(if b.strike { "9" } else { "29" }.into());
    }
    if b.fg != base.fg {
        parts.push(match b.fg {
            Some(Rgb(r, g, bl)) => format!("38;2;{r};{g};{bl}"),
            None => "39".into(),
        });
    }
    if b.bg != base.bg {
        parts.push(match b.bg {
            Some(Rgb(r, g, bl)) => format!("48;2;{r};{g};{bl}"),
            None => "49".into(),
        });
    }
    if b.ul_color != base.ul_color {
        parts.push(match b.ul_color {
            Some(Rgb(r, g, bl)) => format!("58:2::{r}:{g}:{bl}"),
            None => "59".into(),
        });
    }
    if parts.is_empty() {
        return false;
    }
    let _ = write!(out, "{}", parts.join(";"));
    true
}

fn strip_link(s: &Style) -> Style {
    Style { link: 0, ..*s }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tr(a: Style, b: Style) -> String {
        let mut s = String::new();
        sgr_transition(&a, &b, &mut s);
        s
    }
    #[test]
    fn minimal_transitions() {
        let red = Rgb(255, 0, 0);
        assert_eq!(tr(Style::default(), Style::fg(red)), "38;2;255;0;0");
        assert_eq!(tr(Style::fg(red), Style::fg(red)), "");
        assert_eq!(tr(Style::fg(red).bold(), Style::fg(red)), "22");
        // bold off while dim stays on re-asserts dim
        assert_eq!(tr(Style::fg(red).bold().dim(), Style::fg(red).dim()), "22;2");
        // two things off: reset is shorter
        assert_eq!(tr(Style::fg(red).bold().italic(), Style::default()), "0");
        assert_eq!(
            tr(Style::default(), Style::default().underline(Underline::Dotted, Some(red))),
            "4:4;58:2::255:0:0"
        );
    }
}
