use std::ops::Deref;

use gpui::{TestAppContext, VisualTestContext, point};

use super::*;

/// Height of the stand-in column.
///
/// Nothing about the real welcome screen's contents matters here — only that
/// there is a definite height to hold the window against — so the test hands
/// the box one plain child rather than rebuilding the screen.
const COLUMN: f32 = 400.;

/// A window tall enough for the column and both its margins, several times
/// over.
const ROOMY: f32 = 900.;

/// A window shorter than the column, which is the whole point of the box.
const CRAMPED: f32 = 300.;

/// Wide enough that nothing wraps; the box only scrolls one way.
const WIDTH: f32 = 600.;

/// How far apart two measurements may be and still count as the same, in a
/// layout whose lengths are rounded to hundredths of a pixel.
const SLACK: f32 = 0.5;

/// A window holding nothing but the box under test.
struct Harness {
    scroll: ScrollHandle,
    bar: ScrollbarState,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let bar = Scrollbar::for_handle(SCROLLBARS[1].0, Surface::Welcome.axis(), &self.scroll)
            .fade(self.bar.fade());

        div().flex().flex_col().size_full().child(centered_scroll(
            WELCOME_STATE,
            &self.scroll,
            bar,
            &theme,
            div().flex_none().w(px(320.)).h(px(COLUMN)),
        ))
    }
}

/// Opens the harness in a window `height` tall and hands back its handle.
///
/// Drawn twice: a bar is built from the box as the previous frame measured
/// it, so the opening frame has nothing to build one out of.
fn open(cx: &mut TestAppContext, height: f32) -> ScrollHandle {
    let scroll = ScrollHandle::new();
    let window = cx.add_window({
        let scroll = scroll.clone();
        move |_, _| Harness {
            scroll,
            bar: ScrollbarState::new(),
        }
    });

    let mut cx = VisualTestContext::from_window(*window.deref(), cx);
    cx.simulate_resize(gpui::size(px(WIDTH), px(height)));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    scroll
}

/// The bar the workspace would draw over the box as it now stands.
fn scrollbar(scroll: &ScrollHandle) -> Scrollbar {
    Scrollbar::for_handle(SCROLLBARS[1].0, Surface::Welcome.axis(), scroll)
}

/// With room to spare the column sits in the middle, exactly where
/// `justify_center` used to put it, and there is nothing to scroll — so no
/// bar is drawn either.
#[gpui::test]
fn a_column_that_fits_stays_in_the_middle(cx: &mut TestAppContext) {
    let scroll = open(cx, ROOMY);
    let box_ = scroll.bounds();
    let column = scroll
        .bounds_for_item(0)
        .expect("the box never measured its column");

    let above = f32::from(column.top() - box_.top());
    let below = f32::from(box_.bottom() - column.bottom());
    assert!(
        (above - below).abs() < SLACK,
        "the column was not centred: {above} above, {below} below"
    );
    assert_eq!(
        scroll.max_offset().y,
        px(0.),
        "a column that fits left something to scroll"
    );
    assert!(
        scrollbar(&scroll).thumb().is_none(),
        "a box with nothing to scroll drew a bar anyway"
    );
}

/// The regression: with less room than the column needs, the head of it used
/// to be pushed off the top edge and left there. It now starts at the top of
/// the box, and everything past the bottom is reachable by scrolling.
#[gpui::test]
fn a_column_that_does_not_fit_starts_at_the_top(cx: &mut TestAppContext) {
    let scroll = open(cx, CRAMPED);
    let box_ = scroll.bounds();
    let column = scroll
        .bounds_for_item(0)
        .expect("the box never measured its column");

    assert!(
        f32::from(column.top() - box_.top()).abs() < SLACK,
        "the column did not start at the top of the box: {:?} in {:?}",
        column,
        box_
    );
    assert!(
        (f32::from(scroll.max_offset().y) - f32::from(column.size.height - box_.size.height)).abs()
            < SLACK,
        "the scrollable range did not cover the whole of the column"
    );
    assert!(
        scrollbar(&scroll).thumb().is_some(),
        "a box with something to scroll drew no bar"
    );
}

/// And the far end of that scroll reaches the foot of the column, margin and
/// all, rather than stopping short of the last button.
#[gpui::test]
fn scrolling_to_the_end_reaches_the_foot_of_the_column(cx: &mut TestAppContext) {
    let scroll = open(cx, CRAMPED);
    scroll.set_offset(point(px(0.), -scroll.max_offset().y));
    let box_ = scroll.bounds();
    let column = scroll
        .bounds_for_item(0)
        .expect("the box never measured its column");

    let foot = column.bottom() + scroll.offset().y;
    assert!(
        f32::from(foot - box_.bottom()).abs() < SLACK,
        "the end of the scroll left {:?} of the column below the box",
        foot - box_.bottom()
    );
    assert!(
        f32::from(column.size.height) > COLUMN + SCROLL_MARGIN,
        "the column was scrolled to its last button rather than past it"
    );
}
