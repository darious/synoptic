//! Pagination footer (48 h): Previous / Next and `Page 1 of 4 (28 total)`.

use std::rc::Rc;

use gtk::prelude::*;

use super::Browse;
use crate::icons;
use crate::widgets::controls::label;

/// Number of pages for `total` items (at least 1).
pub fn page_count(total: i64, page_size: usize) -> usize {
    if page_size == usize::MAX || page_size == 0 {
        return 1;
    }
    let total = total.max(0) as usize;
    total.div_ceil(page_size).max(1)
}

/// `Page 1 of 4 (28 total)`; `28 total` when unpaginated.
pub fn page_label(page_no: usize, total: i64, page_size: usize) -> String {
    let total = total.max(0);
    if page_size == usize::MAX {
        return format!("{total} total");
    }
    format!(
        "Page {} of {} ({total} total)",
        page_no + 1,
        page_count(total, page_size)
    )
}

pub fn build(ctx: &Rc<Browse>) -> gtk::Widget {
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    footer.add_css_class("pagination");
    footer.set_valign(gtk::Align::End);

    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    inner.set_halign(gtk::Align::Center);
    inner.set_hexpand(true);
    inner.set_valign(gtk::Align::Center);

    let prev = gtk::Button::new();
    {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.append(&icons::image("chevron_left", 16));
        row.append(&gtk::Label::new(Some("Previous")));
        prev.set_child(Some(&row));
    }
    prev.add_css_class("page-btn");
    prev.set_sensitive(false);
    let text = label("", &["page-label"]);
    text.set_xalign(0.5);
    let next = gtk::Button::new();
    {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.append(&gtk::Label::new(Some("Next")));
        row.append(&icons::image("chevron_right", 16));
        next.set_child(Some(&row));
    }
    next.add_css_class("page-btn");
    next.set_sensitive(false);

    let unpaginated = ctx.page_size == usize::MAX;
    prev.set_visible(!unpaginated);
    next.set_visible(!unpaginated);

    inner.append(&prev);
    inner.append(&text);
    inner.append(&next);
    footer.append(&inner);

    {
        let ctx = ctx.clone();
        prev.connect_clicked(move |_| {
            let n = ctx.filter.borrow().page_no;
            if n > 0 {
                ctx.set_page(n - 1);
            }
        });
    }
    {
        let ctx = ctx.clone();
        next.connect_clicked(move |_| {
            let n = ctx.filter.borrow().page_no;
            ctx.set_page(n + 1);
        });
    }

    {
        let ctx_w = Rc::downgrade(ctx);
        let (prev, next, text) = (prev.clone(), next.clone(), text.clone());
        ctx.on_loaded(move |total| {
            let Some(ctx) = ctx_w.upgrade() else { return };
            let page_no = ctx.filter.borrow().page_no;
            let pages = page_count(total, ctx.page_size);
            // The current page vanished (e.g. after a delete): go to the
            // last one that still exists.
            if total > 0 && page_no >= pages {
                ctx.set_page(pages - 1);
                return;
            }
            text.set_text(&page_label(page_no, total, ctx.page_size));
            prev.set_sensitive(page_no > 0);
            next.set_sensitive(page_no + 1 < pages);
        });
    }

    footer.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_labels() {
        assert_eq!(page_count(0, 8), 1);
        assert_eq!(page_count(8, 8), 1);
        assert_eq!(page_count(9, 8), 2);
        assert_eq!(page_count(28, 8), 4);
        assert_eq!(page_count(28, usize::MAX), 1);
        assert_eq!(page_label(0, 28, 8), "Page 1 of 4 (28 total)");
        assert_eq!(page_label(3, 28, 8), "Page 4 of 4 (28 total)");
        assert_eq!(page_label(0, 0, 9), "Page 1 of 1 (0 total)");
        assert_eq!(page_label(0, 12, usize::MAX), "12 total");
    }
}
