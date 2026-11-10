use std::collections::VecDeque;

use crate::display::Rect;

/// The rectangles a buffer holding frame `held` must copy from the shown
/// frame before frame `damage` is painted into it: what each later frame in
/// `history` changed, or all of `whole` when the history no longer reaches
/// back to `held`, less what `damage` repaints, with no pixel named twice.
pub(crate) fn missing(
    history: &VecDeque<(u64, Vec<Rect>)>,
    held: Option<u64>,
    whole: Rect,
    damage: &[Rect],
) -> Vec<Rect> {
    let reaches = held.filter(|held| history.front().is_some_and(|(first, _)| *first <= held + 1));
    let changed: Vec<Rect> = match reaches {
        Some(held) => history
            .iter()
            .filter(|(frame, _)| *frame > held)
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect(),
        None => vec![whole],
    };
    let mut copies: Vec<Rect> = Vec::new();
    for rect in changed {
        let mut pieces = vec![rect];
        for cut in damage.iter().chain(&copies) {
            pieces = pieces
                .into_iter()
                .flat_map(|piece| subtract(piece, *cut))
                .collect();
        }
        copies.extend(pieces);
    }
    copies
}

// The parts of `a` outside `b`: at most a band above, a band below, and the
// pieces left and right of `b` between them.
fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let (ax2, ay2) = (a.x + a.width, a.y + a.height);
    let (bx2, by2) = (b.x + b.width, b.y + b.height);
    let (top, bottom) = (b.y.max(a.y), by2.min(ay2));
    let (left, right) = (b.x.max(a.x), bx2.min(ax2));
    if a.width <= 0 || a.height <= 0 {
        return Vec::new();
    }
    if left >= right || top >= bottom {
        return vec![a];
    }
    let band = |x: i64, y: i64, x2: i64, y2: i64| {
        (x2 > x && y2 > y).then_some(Rect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        })
    };
    [
        band(a.x, a.y, ax2, top),
        band(a.x, bottom, ax2, ay2),
        band(a.x, top, left, bottom),
        band(right, top, ax2, bottom),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::missing;
    use crate::display::Rect;

    fn rect(x: i64, y: i64, width: i64, height: i64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn area(rects: &[Rect]) -> i64 {
        rects.iter().map(|r| r.width * r.height).sum()
    }

    fn covers(rects: &[Rect], x: i64, y: i64) -> usize {
        rects
            .iter()
            .filter(|r| (r.x..r.x + r.width).contains(&x) && (r.y..r.y + r.height).contains(&y))
            .count()
    }

    #[test]
    fn a_frame_that_repaints_what_earlier_frames_changed_copies_nothing() {
        let whole = rect(0, 0, 100, 80);
        let history = VecDeque::from([(1, vec![whole]), (2, vec![whole])]);
        assert!(missing(&history, Some(0), whole, &[whole]).is_empty());
    }

    #[test]
    fn a_buffer_without_a_frame_copies_all_the_current_frame_leaves_alone() {
        let whole = rect(0, 0, 100, 80);
        let history = VecDeque::new();
        let damage = [rect(0, 0, 100, 20)];
        let copies = missing(&history, None, whole, &damage);
        assert_eq!(area(&copies), 100 * 60);
        assert_eq!(covers(&copies, 5, 10), 0);
        assert_eq!(covers(&copies, 5, 50), 1);
    }

    #[test]
    fn overlapping_changes_copy_each_pixel_once_outside_the_damage() {
        let whole = rect(0, 0, 100, 80);
        let history = VecDeque::from([
            (3, vec![rect(0, 0, 50, 50)]),
            (4, vec![rect(25, 25, 50, 50)]),
            (5, vec![rect(90, 70, 5, 5)]),
        ]);
        let damage = [rect(40, 40, 10, 10)];
        let copies = missing(&history, Some(3), whole, &damage);
        for y in 0..80 {
            for x in 0..100 {
                let changed = (25..75).contains(&x) && (25..75).contains(&y)
                    || (90..95).contains(&x) && (70..75).contains(&y);
                let painted = (40..50).contains(&x) && (40..50).contains(&y);
                let want = usize::from(changed && !painted);
                assert_eq!(covers(&copies, x, y), want, "{x},{y}");
            }
        }
    }

    #[test]
    fn a_history_that_no_longer_reaches_the_held_frame_copies_the_whole_frame() {
        let whole = rect(0, 0, 10, 10);
        let history = VecDeque::from([(7, vec![rect(0, 0, 1, 1)])]);
        assert_eq!(missing(&history, Some(2), whole, &[]), vec![whole]);
    }
}
