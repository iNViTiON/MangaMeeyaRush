//! Core navigation model: a loaded "book" (PageSource) with a cursor, spread
//! pairing logic, and fit/zoom state.

use std::path::Path;
use std::sync::Arc;

use mmce_codecs::{open_source, CodecError, PageSource};
use mmce_config::{BindDir, FitMode, PageMode, Settings};

pub use mmce_codecs;
pub use mmce_config;

/// A loaded source plus the current viewing cursor.
pub struct Book {
    source: Arc<dyn PageSource>,
    cursor: usize,
}

impl Book {
    pub fn open(path: &Path) -> Result<Self, CodecError> {
        let source: Arc<dyn PageSource> = Arc::from(open_source(path)?);
        Ok(Self { source, cursor: 0 })
    }

    pub fn source(&self) -> &Arc<dyn PageSource> {
        &self.source
    }

    pub fn len(&self) -> usize {
        self.source.len()
    }

    pub fn is_empty(&self) -> bool {
        self.source.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn title(&self) -> &str {
        self.source.name()
    }

    pub fn goto(&mut self, idx: usize) {
        if self.source.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = idx.min(self.source.len() - 1);
        }
    }

    pub fn first(&mut self) {
        self.cursor = 0;
    }

    pub fn last(&mut self) {
        if !self.source.is_empty() {
            self.cursor = self.source.len() - 1;
        }
    }

    /// Move by an arbitrary signed page delta, clamped to `[0, len-1]`.
    pub fn advance(&mut self, delta: isize) {
        if self.source.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = (self.source.len() - 1) as isize;
        let new = (self.cursor as isize + delta).clamp(0, last);
        self.cursor = new as usize;
    }

    /// Move forward by a spread (1 or 2 pages depending on mode).
    ///
    /// For `PageMode::Auto` the caller should resolve to Single or Spread
    /// first; passing Auto here treats it like Spread.
    pub fn next_spread(&mut self, mode: PageMode) {
        let step = stride(mode);
        self.advance(step as isize);
    }

    /// Move backward by a spread.
    pub fn prev_spread(&mut self, mode: PageMode) {
        let step = stride(mode);
        self.advance(-(step as isize));
    }

    /// Nudge one page forward (Shift+→).
    pub fn next_page(&mut self) {
        self.advance(1);
    }

    /// Nudge one page backward (Shift+←).
    pub fn prev_page(&mut self) {
        self.advance(-1);
    }

    /// Compute the two page indices for the current spread, in *display order*
    /// left-to-right. For RTL (manga) the right page is shown on the left of
    /// the pair when laid out linearly, so we flip.
    pub fn current_spread(&self, mode: PageMode, dir: BindDir) -> Spread {
        if self.is_empty() {
            return Spread::default();
        }
        let a = self.cursor;
        let want_pair = matches!(mode, PageMode::Spread | PageMode::Auto);
        let b = if want_pair && a + 1 < self.len() {
            Some(a + 1)
        } else {
            None
        };
        match (b, dir) {
            (None, _) => Spread {
                left: Some(a),
                right: None,
            },
            (Some(b), BindDir::LeftToRight) => Spread {
                left: Some(a),
                right: Some(b),
            },
            (Some(b), BindDir::RightToLeft) => Spread {
                left: Some(b),
                right: Some(a),
            },
        }
    }
}

/// Pages covered by a single forward/backward step in the given mode.
pub fn stride(mode: PageMode) -> usize {
    match mode {
        PageMode::Single => 1,
        PageMode::Spread | PageMode::Auto => 2,
    }
}

/// Which page index lives on the left and right of the current spread, in
/// physical display order.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

impl Spread {
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        [self.left, self.right].into_iter().flatten()
    }
}

/// Viewer state carried by the app and tweaked per key/mouse event.
#[derive(Debug, Clone)]
pub struct ViewerState {
    pub fit: FitMode,
    pub zoom: f32,
    pub pan: [f32; 2],
    pub page_mode: PageMode,
    pub bind_dir: BindDir,
    pub fullscreen: bool,
    pub bg_color: u32,
}

impl ViewerState {
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            fit: s.scale.mode,
            zoom: s.scale.optional_scale.max(0.05),
            pan: [0.0, 0.0],
            page_mode: s.view.page_mode,
            bind_dir: s.view.bind_dir,
            fullscreen: s.general.fullscreen,
            bg_color: s.general.bg_color,
        }
    }

    /// Cycle Single → Spread → Auto → Single. Called on Space.
    pub fn toggle_page_mode(&mut self) {
        self.page_mode = match self.page_mode {
            PageMode::Single => PageMode::Spread,
            PageMode::Spread => PageMode::Auto,
            PageMode::Auto => PageMode::Single,
        };
    }

    pub fn zoom_in(&mut self) {
        self.fit = FitMode::Custom;
        self.zoom = (self.zoom * 1.25).min(16.0);
    }

    pub fn zoom_out(&mut self) {
        self.fit = FitMode::Custom;
        self.zoom = (self.zoom / 1.25).max(0.05);
    }

    pub fn reset_zoom(&mut self) {
        self.fit = FitMode::Fit;
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmce_codecs::PageSource;
    use std::sync::Arc;

    struct Fake(usize);
    impl PageSource for Fake {
        fn len(&self) -> usize {
            self.0
        }
        fn name(&self) -> &str {
            "fake"
        }
        fn entry_name(&self, _: usize) -> Option<&str> {
            None
        }
        fn read(&self, _: usize) -> Result<Vec<u8>, CodecError> {
            unreachable!()
        }
    }

    fn book(n: usize) -> Book {
        Book {
            source: Arc::new(Fake(n)),
            cursor: 0,
        }
    }

    #[test]
    fn spread_rtl_flips_order() {
        let b = book(10);
        let s = b.current_spread(PageMode::Spread, BindDir::RightToLeft);
        assert_eq!(s.left, Some(1));
        assert_eq!(s.right, Some(0));
    }

    #[test]
    fn spread_ltr_natural_order() {
        let b = book(10);
        let s = b.current_spread(PageMode::Spread, BindDir::LeftToRight);
        assert_eq!(s.left, Some(0));
        assert_eq!(s.right, Some(1));
    }

    #[test]
    fn single_only_returns_left() {
        let b = book(10);
        let s = b.current_spread(PageMode::Single, BindDir::LeftToRight);
        assert_eq!(s.left, Some(0));
        assert_eq!(s.right, None);
    }

    #[test]
    fn next_spread_steps_two_in_spread_mode() {
        let mut b = book(10);
        b.next_spread(PageMode::Spread);
        assert_eq!(b.cursor, 2);
        b.next_spread(PageMode::Spread);
        assert_eq!(b.cursor, 4);
    }

    #[test]
    fn shift_step_always_advances_one_even_in_spread() {
        // Analogue of user's concern: in 2-up manga spread, Shift+arrow
        // should step by a single image so the left page becomes the right
        // page (RTL) or vice versa.
        let mut b = book(10);
        b.cursor = 2;

        // Initial RTL spread showing (L=3, R=2).
        let s0 = b.current_spread(PageMode::Spread, BindDir::RightToLeft);
        assert_eq!(s0.left, Some(3));
        assert_eq!(s0.right, Some(2));

        // Shift+←  (manga-forward) advances book cursor by 1.
        b.advance(1);
        assert_eq!(b.cursor, 3);

        // New spread: page 3 has moved from the left slot to the right slot.
        let s1 = b.current_spread(PageMode::Spread, BindDir::RightToLeft);
        assert_eq!(s1.left, Some(4));
        assert_eq!(s1.right, Some(3));

        // Shift+→ (manga-back) moves back by one.
        b.advance(-1);
        assert_eq!(b.cursor, 2);
        let s2 = b.current_spread(PageMode::Spread, BindDir::RightToLeft);
        assert_eq!(s2, s0);
    }

    #[test]
    fn next_spread_clamps_to_last() {
        let mut b = book(3);
        b.cursor = 2;
        b.next_spread(PageMode::Spread);
        assert_eq!(b.cursor, 2);
    }

    #[test]
    fn prev_spread_clamps_to_zero() {
        let mut b = book(3);
        b.prev_spread(PageMode::Spread);
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn empty_book_is_noop() {
        let mut b = book(0);
        b.next_spread(PageMode::Spread);
        b.prev_spread(PageMode::Spread);
        assert_eq!(b.cursor, 0);
        assert_eq!(b.current_spread(PageMode::Spread, BindDir::RightToLeft), Spread::default());
    }
}
