//! The editor's frame, cell by cell, into tuicr's.
//!
//! Both sides keep a grid of styled cells, so this is a copy with three
//! translations: the names of the sixteen terminal colours, which the two
//! libraries spell differently for the greys; the modifier bits, which are
//! the same; and the underline, which the editor has in five styles and
//! ratatui in one.

use helix_embed as hx;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

/// Copy `frame` into `area` of `buf`, top-left corner on top-left corner.
///
/// Only the cells both have are written: a frame larger than the area is
/// cut off, and cells of the area beyond the frame are left as they were.
pub(super) fn blit(frame: &hx::Buffer, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    let frame_width = usize::from(frame.area.width);
    let columns = area.width.min(frame.area.width);
    let rows = area.height.min(frame.area.height);
    for row in 0..rows {
        let start = usize::from(row) * frame_width;
        for column in 0..columns {
            let Some(source) = frame.content.get(start + usize::from(column)) else {
                return;
            };
            let Some(cell) = buf.cell_mut((area.x + column, area.y + row)) else {
                continue;
            };
            // Whatever tuicr drew here before the editor took the screen.
            cell.reset();
            cell.set_symbol(source.symbol.as_str());
            cell.fg = color(source.fg);
            cell.bg = color(source.bg);
            cell.underline_color = color(source.underline_color);
            cell.modifier = modifier(source.modifier, source.underline_style);
        }
    }
}

/// The editor's colour in ratatui's names.
///
/// The greys are where the names part ways. The editor calls bright black
/// `Gray`, plain white `LightGray` and bright white `White`; ratatui calls
/// them `DarkGray`, `Gray` and `White`.
pub(super) fn color(color: hx::Color) -> Color {
    match color {
        hx::Color::Reset => Color::Reset,
        hx::Color::Black => Color::Black,
        hx::Color::Red => Color::Red,
        hx::Color::Green => Color::Green,
        hx::Color::Yellow => Color::Yellow,
        hx::Color::Blue => Color::Blue,
        hx::Color::Magenta => Color::Magenta,
        hx::Color::Cyan => Color::Cyan,
        hx::Color::Gray => Color::DarkGray,
        hx::Color::LightRed => Color::LightRed,
        hx::Color::LightGreen => Color::LightGreen,
        hx::Color::LightYellow => Color::LightYellow,
        hx::Color::LightBlue => Color::LightBlue,
        hx::Color::LightMagenta => Color::LightMagenta,
        hx::Color::LightCyan => Color::LightCyan,
        hx::Color::LightGray => Color::Gray,
        hx::Color::White => Color::White,
        hx::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        hx::Color::Indexed(index) => Color::Indexed(index),
    }
}

/// The editor's modifiers as ratatui's. The bits are the same ones; the
/// editor keeps the underline apart, in five styles (line, curl, dotted,
/// dashed, double), and ratatui's cell has the one, so every style is
/// drawn as a plain underline. Its colour is copied separately.
pub(super) fn modifier(modifier: hx::Modifier, underline: hx::UnderlineStyle) -> Modifier {
    let mut out = Modifier::from_bits_truncate(modifier.bits());
    // The editor has no underline bit of its own; nothing it sets may be
    // read as one.
    out.remove(Modifier::UNDERLINED);
    if underline != hx::UnderlineStyle::Reset {
        out.insert(Modifier::UNDERLINED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u16, height: u16) -> hx::Buffer {
        hx::Buffer::empty(hx::Rect::new(0, 0, width, height))
    }

    #[test]
    fn should_copy_text_to_the_area_s_corner_and_leave_the_rest_alone() {
        let mut source = frame(4, 2);
        for (column, symbol) in ["a", "b", "c", "d"].into_iter().enumerate() {
            source[(column as u16, 0)].set_symbol(symbol);
            source[(column as u16, 1)].set_symbol(&symbol.to_uppercase());
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 4));
        buf.set_string(0, 0, "tttttttt", ratatui::style::Style::default());

        blit(&source, Rect::new(2, 1, 4, 2), &mut buf);

        let row = |y: u16| -> String { (0..8).map(|x| buf[(x, y)].symbol()).collect() };
        assert_eq!(row(0), "tttttttt", "above the area");
        assert_eq!(row(1), "  abcd  ");
        assert_eq!(row(2), "  ABCD  ");
        assert_eq!(row(3), "        ");
    }

    #[test]
    fn should_replace_whatever_style_the_cell_had() {
        let source = frame(1, 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        buf[(0, 0)]
            .set_symbol("x")
            .set_fg(Color::Red)
            .set_bg(Color::Blue);
        buf[(0, 0)].modifier = Modifier::BOLD | Modifier::UNDERLINED;

        blit(&source, Rect::new(0, 0, 1, 1), &mut buf);

        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), " ");
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
        assert_eq!(cell.modifier, Modifier::empty());
    }

    #[test]
    fn should_copy_colours_modifiers_and_the_underline_colour() {
        let mut source = frame(2, 1);
        {
            let cell = &mut source[(0, 0)];
            cell.set_symbol("k");
            cell.fg = hx::Color::Rgb(1, 2, 3);
            cell.bg = hx::Color::Indexed(200);
            cell.underline_color = hx::Color::LightRed;
            cell.underline_style = hx::UnderlineStyle::Curl;
            cell.modifier = hx::Modifier::BOLD | hx::Modifier::ITALIC;
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));

        blit(&source, Rect::new(0, 0, 2, 1), &mut buf);

        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "k");
        assert_eq!(cell.fg, Color::Rgb(1, 2, 3));
        assert_eq!(cell.bg, Color::Indexed(200));
        assert_eq!(cell.underline_color, Color::LightRed);
        assert_eq!(
            cell.modifier,
            Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED
        );
        // The neighbour got none of it.
        assert_eq!(buf[(1, 0)].modifier, Modifier::empty());
        assert_eq!(buf[(1, 0)].underline_color, Color::Reset);
    }

    #[test]
    fn should_cut_a_frame_larger_than_the_area_and_stay_inside_the_buffer() {
        let mut source = frame(6, 3);
        for row in 0..3 {
            for column in 0..6 {
                source[(column, row)].set_symbol("#");
            }
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 3));

        // The area is smaller than the frame, and hangs over the buffer.
        blit(&source, Rect::new(3, 1, 4, 5), &mut buf);

        let row = |y: u16| -> String { (0..5).map(|x| buf[(x, y)].symbol()).collect() };
        assert_eq!(row(0), "     ");
        assert_eq!(row(1), "   ##");
        assert_eq!(row(2), "   ##");
    }

    #[test]
    fn should_leave_cells_beyond_a_smaller_frame_as_they_were() {
        let mut source = frame(2, 1);
        source[(0, 0)].set_symbol("e");
        source[(1, 0)].set_symbol("d");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        buf.set_string(0, 0, "....", ratatui::style::Style::default());
        buf.set_string(0, 1, "....", ratatui::style::Style::default());

        blit(&source, Rect::new(0, 0, 4, 2), &mut buf);

        let row = |y: u16| -> String { (0..4).map(|x| buf[(x, y)].symbol()).collect() };
        assert_eq!(row(0), "ed..");
        assert_eq!(row(1), "....");
    }

    #[test]
    fn should_keep_a_wide_character_in_one_cell() {
        let mut source = frame(3, 1);
        source.set_string(0, 0, "\u{6f22}x", Default::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));

        blit(&source, Rect::new(0, 0, 3, 1), &mut buf);

        assert_eq!(buf[(0, 0)].symbol(), "\u{6f22}");
        assert_eq!(buf[(2, 0)].symbol(), "x");
    }

    #[test]
    fn should_spell_the_greys_as_ratatui_does() {
        // Bright black, white and bright white: ANSI colours 8, 7 and 15.
        assert_eq!(color(hx::Color::Gray), Color::DarkGray);
        assert_eq!(color(hx::Color::LightGray), Color::Gray);
        assert_eq!(color(hx::Color::White), Color::White);
        assert_eq!(color(hx::Color::Black), Color::Black);
    }

    #[test]
    fn should_keep_every_other_colour_s_name() {
        let same = [
            (hx::Color::Reset, Color::Reset),
            (hx::Color::Red, Color::Red),
            (hx::Color::Green, Color::Green),
            (hx::Color::Yellow, Color::Yellow),
            (hx::Color::Blue, Color::Blue),
            (hx::Color::Magenta, Color::Magenta),
            (hx::Color::Cyan, Color::Cyan),
            (hx::Color::LightRed, Color::LightRed),
            (hx::Color::LightGreen, Color::LightGreen),
            (hx::Color::LightYellow, Color::LightYellow),
            (hx::Color::LightBlue, Color::LightBlue),
            (hx::Color::LightMagenta, Color::LightMagenta),
            (hx::Color::LightCyan, Color::LightCyan),
            (hx::Color::Rgb(250, 128, 0), Color::Rgb(250, 128, 0)),
            (hx::Color::Indexed(17), Color::Indexed(17)),
        ];
        for (theirs, ours) in same {
            assert_eq!(color(theirs), ours, "{theirs:?}");
        }
    }

    #[test]
    fn should_copy_each_modifier_bit_to_the_same_modifier() {
        let pairs = [
            (hx::Modifier::BOLD, Modifier::BOLD),
            (hx::Modifier::DIM, Modifier::DIM),
            (hx::Modifier::ITALIC, Modifier::ITALIC),
            (hx::Modifier::SLOW_BLINK, Modifier::SLOW_BLINK),
            (hx::Modifier::RAPID_BLINK, Modifier::RAPID_BLINK),
            (hx::Modifier::REVERSED, Modifier::REVERSED),
            (hx::Modifier::HIDDEN, Modifier::HIDDEN),
            (hx::Modifier::CROSSED_OUT, Modifier::CROSSED_OUT),
        ];
        for (theirs, ours) in pairs {
            assert_eq!(
                modifier(theirs, hx::UnderlineStyle::Reset),
                ours,
                "{theirs:?}"
            );
        }
        assert_eq!(
            modifier(hx::Modifier::all(), hx::UnderlineStyle::Reset),
            Modifier::all() - Modifier::UNDERLINED
        );
        assert_eq!(
            modifier(hx::Modifier::empty(), hx::UnderlineStyle::Reset),
            Modifier::empty()
        );
    }

    #[test]
    fn should_draw_every_underline_style_as_an_underline() {
        for style in [
            hx::UnderlineStyle::Line,
            hx::UnderlineStyle::Curl,
            hx::UnderlineStyle::Dotted,
            hx::UnderlineStyle::Dashed,
            hx::UnderlineStyle::DoubleLine,
        ] {
            assert_eq!(
                modifier(hx::Modifier::empty(), style),
                Modifier::UNDERLINED,
                "{style:?}"
            );
            assert_eq!(
                modifier(hx::Modifier::BOLD, style),
                Modifier::BOLD | Modifier::UNDERLINED
            );
        }
    }
}
