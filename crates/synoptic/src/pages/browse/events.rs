//! Events page: `EventCenter.Event.List` entries as 56 px rows
//! (`4px 110px 1fr auto`) with the type bar, type icon + label, camera,
//! mono range, duration tag and Play. The NAS has no type filter or paging
//! for this list, so the whole range is fetched once per filter and typed /
//! paged client-side (page size 8).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use ss_api::{EventCenterEvent, Recording};

use super::{
    Browse, Content, EventType, clear_box, format_duration, format_range, hint, parent_window,
};
use crate::icons;
use crate::widgets::controls::{label, spacer};

/// What an Event Center entry is, for display and the type filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Typed(EventType),
    /// Any other type the NAS reports (intrusion, face, smoke, …).
    Other(String),
}

impl EventKind {
    pub fn label(&self) -> String {
        match self {
            EventKind::Typed(t) => t.label().to_string(),
            EventKind::Other(s) => s.clone(),
        }
    }

    /// Suffix of the `type-*` / `bg-*` CSS classes.
    pub fn css(&self) -> &'static str {
        match self {
            EventKind::Typed(t) => t.css(),
            EventKind::Other(_) => "other",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            EventKind::Typed(t) => t.icon(),
            EventKind::Other(_) => "event",
        }
    }

    /// Passes the Events page's type pill (`None` = every type).
    pub fn matches(&self, filter: Option<EventType>) -> bool {
        match filter {
            None => true,
            Some(t) => *self == EventKind::Typed(t),
        }
    }
}

/// `EventCenter.Event`'s `event_type` / `object_type` → kind. Numbers are
/// Synology's own enums (see the reference client's `event_center.py`).
pub fn classify(event_type: i64, object_type: i64) -> EventKind {
    match event_type {
        10 => EventKind::Typed(EventType::Motion),
        11 => match object_type {
            2 | 4 => EventKind::Typed(EventType::People),
            3 => EventKind::Typed(EventType::Vehicle),
            1 => EventKind::Other("Animal".into()),
            _ => EventKind::Other("Object".into()),
        },
        12 => EventKind::Typed(EventType::Tampering),
        18 => EventKind::Typed(EventType::Audio),
        other => EventKind::Other(
            match other {
                1 => "Intrusion",
                2 => "Loitering",
                3 => "License plate",
                4 => "Crowd",
                5 => "Vacancy",
                6 => "Tailgating",
                7 => "Running",
                8 => "Face detection",
                9 => "Face recognition",
                13 => "Face temperature",
                14 => "Temperature",
                15 => "High temperature",
                16 => "Low temperature",
                17 => "Smoke",
                19 => "Digital input",
                20 => "External input",
                21 => "Intercom",
                22 => "PIR",
                23 => "Unattended object",
                24 => "Missing object",
                25 => "Enter",
                26 => "Exit",
                _ => return EventKind::Other(format!("Type {other}")),
            }
            .to_string(),
        ),
    }
}

/// One rendered event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub camera_id: i64,
    pub start: i64,
    pub duration: i64,
    pub kind: EventKind,
}

impl From<&EventCenterEvent> for EventRow {
    fn from(e: &EventCenterEvent) -> Self {
        EventRow {
            id: e.id,
            camera_id: e.camera_id,
            start: e.start_time,
            duration: e.duration.max(0),
            kind: classify(e.event_type, e.object_type),
        }
    }
}

/// The rows of page `page_no` after the type filter, and the filtered total.
pub fn page_of(
    rows: &[EventRow],
    filter: Option<EventType>,
    page_no: usize,
    page_size: usize,
) -> (Vec<&EventRow>, usize) {
    let filtered: Vec<&EventRow> = rows.iter().filter(|r| r.kind.matches(filter)).collect();
    let total = filtered.len();
    let start = (page_no * page_size).min(total);
    let end = (start + page_size).min(total);
    (filtered[start..end].to_vec(), total)
}

type CacheKey = (i64, i64, Vec<i64>);
/// The last fetched range and its rows, for client-side paging.
type Cache = Rc<RefCell<Option<(CacheKey, Vec<EventRow>)>>>;

/// Resolve the recording file holding the event and open the player there.
fn play(ctx: &Rc<Browse>, anchor: &gtk::Widget, ev: &EventRow) {
    let Some(window) = parent_window(anchor) else {
        return;
    };
    let Some(client) = ctx.client() else {
        ctx.toast("Not connected");
        return;
    };
    let (camera_id, start, duration) = (ev.camera_id, ev.start, ev.duration.max(1));
    let fut = ctx.services.run(async move {
        client
            .enum_interval(&[camera_id], start, start + duration)
            .await
    });
    let ctx = ctx.clone();
    let camera_name = ctx.camera_name(camera_id).unwrap_or_default();
    glib::spawn_future_local(async move {
        let cams = match fut.await {
            Ok(c) => c,
            Err(e) => {
                ctx.toast(format!("Could not find the recording: {e}"));
                return;
            }
        };
        let found = cams.iter().find_map(|cam| {
            cam.recordings
                .iter()
                .find(|r| r.start <= start && start < r.stop.max(r.start + 1))
                .map(|r| (cam.mount_id, cam.arch_id, r.id, r.start, r.stop))
        });
        let Some((mount_id, arch_id, id, rec_start, rec_stop)) = found else {
            ctx.toast("No recording covers this event");
            return;
        };
        let rec = Recording {
            id,
            camera_id,
            camera_name,
            start_time: rec_start,
            stop_time: rec_stop,
            file_size: 0,
            // Not the event classification: EventStream rejects it as
            // recEvtType (see the reference client).
            event_type: 0,
            mount_id,
            arch_id,
            detection_label: 0,
        };
        let offset = (start - rec_start).max(0) as f64;
        crate::dialogs::player::open(&window, &ctx.store, &ctx.services, rec, Some(offset));
    });
}

fn row(ctx: &Rc<Browse>, ev: &EventRow) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("list-row");
    row.add_css_class("event-row");
    row.add_css_class("clickable");

    let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bar.add_css_class("event-bar");
    bar.add_css_class(&format!("bg-{}", ev.kind.css()));
    bar.set_vexpand(true);
    row.append(&bar);

    let type_col = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    type_col.add_css_class("event-type");
    type_col.set_valign(gtk::Align::Center);
    let icon = icons::image(ev.kind.icon(), 18);
    icon.add_css_class(&format!("type-{}", ev.kind.css()));
    type_col.append(&icon);
    let tl = label(&ev.kind.label(), &["type-label"]);
    tl.add_css_class(&format!("type-{}", ev.kind.css()));
    tl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    type_col.append(&tl);
    row.append(&type_col);

    let cam = label(
        &ctx.camera_name(ev.camera_id)
            .unwrap_or_else(|| format!("Camera {}", ev.camera_id)),
        &["event-cam"],
    );
    cam.set_ellipsize(gtk::pango::EllipsizeMode::End);
    cam.set_valign(gtk::Align::Center);
    row.append(&cam);

    let range = label(
        &format_range(ev.start, ev.start + ev.duration, " – "),
        &["row-range"],
    );
    range.set_valign(gtk::Align::Center);
    row.append(&range);

    let dur = label(&format_duration(ev.duration), &["duration-tag"]);
    dur.set_valign(gtk::Align::Center);
    row.append(&dur);

    row.append(&spacer());

    let play_btn = icons::bordered_button("play_arrow_fill", "icon-34", "Play");
    play_btn.add_css_class("row-actions");
    {
        let ctx = ctx.clone();
        let ev = ev.clone();
        play_btn.connect_clicked(move |b| play(&ctx, b.upcast_ref(), &ev));
    }
    row.append(&play_btn);

    // open the Live page with the timeline at the event
    let timeline_btn =
        icons::bordered_button("view_timeline", "icon-34", "Show on the Live View timeline");
    timeline_btn.add_css_class("row-actions");
    timeline_btn.set_widget_name("ev-timeline");
    {
        let store = ctx.store.clone();
        let (start, camera_id) = (ev.start, ev.camera_id);
        timeline_btn.connect_clicked(move |_| {
            store.emit(&crate::state::Change::RequestSeek(start, camera_id));
        });
    }
    row.append(&timeline_btn);

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    {
        let ctx = ctx.clone();
        let ev = ev.clone();
        let row_w = row.clone();
        click.connect_released(move |g, _, x, y| {
            if row_w.contains(x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                play(&ctx, row_w.upcast_ref(), &ev);
            }
        });
    }
    row.add_controller(click);

    row.upcast()
}

fn render(ctx: &Rc<Browse>, list: &gtk::Box, rows: &[EventRow]) {
    clear_box(list);
    let (filter, page_no) = {
        let f = ctx.filter.borrow();
        (f.event_type, f.page_no)
    };
    let (page, total) = page_of(rows, filter, page_no, ctx.page_size);
    if page.is_empty() {
        list.append(&hint("No events match the current filter."));
    }
    for ev in page {
        list.append(&row(ctx, ev));
    }
    ctx.loaded(total as i64);
}

pub fn build(ctx: &Rc<Browse>) -> Content {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("browse-list");
    scroller.set_child(Some(&list));

    let cache: Cache = Rc::new(RefCell::new(None));

    let load: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let list = list.clone();
        Rc::new(move || {
            let generation = ctx.generation();
            let force = ctx.take_force();
            let Some(client) = ctx.client() else {
                clear_box(&list);
                list.append(&hint("Not connected. Events appear after login."));
                *cache.borrow_mut() = None;
                ctx.loaded(0);
                return;
            };
            let (from, to) = ctx
                .time_range_unix()
                .unwrap_or_else(|| (0, chrono::Local::now().timestamp()));
            let camera_ids = ctx.camera_ids();
            let key: CacheKey = (from, to, camera_ids.clone());
            if !force
                && let Some((k, rows)) = cache.borrow().as_ref()
                && *k == key
            {
                render(&ctx, &list, rows);
                return;
            }
            clear_box(&list);
            list.append(&hint("Loading events…"));
            if camera_ids.is_empty() {
                clear_box(&list);
                list.append(&hint("No cameras."));
                ctx.loaded(0);
                return;
            }
            let fut = ctx
                .services
                .run(async move { client.list_event_center_events(&camera_ids, from, to).await });
            let ctx = ctx.clone();
            let list = list.clone();
            let cache = cache.clone();
            glib::spawn_future_local(async move {
                let result = fut.await;
                if !ctx.is_current(generation) {
                    return;
                }
                match result {
                    Ok(events) => {
                        let mut rows: Vec<EventRow> = events.iter().map(EventRow::from).collect();
                        rows.sort_by(|a, b| b.start.cmp(&a.start).then(b.id.cmp(&a.id)));
                        render(&ctx, &list, &rows);
                        *cache.borrow_mut() = Some((key, rows));
                    }
                    Err(e) => {
                        clear_box(&list);
                        list.append(&hint(&format!("Could not load events: {e}")));
                        *cache.borrow_mut() = None;
                        ctx.loaded(0);
                    }
                }
            });
        })
    };

    Content {
        widget: scroller.upcast(),
        load,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify(10, 0), EventKind::Typed(EventType::Motion));
        assert_eq!(classify(11, 2), EventKind::Typed(EventType::People));
        assert_eq!(classify(11, 4), EventKind::Typed(EventType::People));
        assert_eq!(classify(11, 3), EventKind::Typed(EventType::Vehicle));
        assert_eq!(classify(11, 1), EventKind::Other("Animal".into()));
        assert_eq!(classify(11, 0), EventKind::Other("Object".into()));
        assert_eq!(classify(12, 0), EventKind::Typed(EventType::Tampering));
        assert_eq!(classify(18, 0), EventKind::Typed(EventType::Audio));
        assert_eq!(classify(17, 0), EventKind::Other("Smoke".into()));
        assert_eq!(classify(99, 0), EventKind::Other("Type 99".into()));
        assert_eq!(classify(99, 0).css(), "other");
        assert_eq!(classify(10, 0).css(), "motion");
    }

    fn row(id: i64, start: i64, kind: EventKind) -> EventRow {
        EventRow {
            id,
            camera_id: 1,
            start,
            duration: 10,
            kind,
        }
    }

    #[test]
    fn paging_and_type_filter() {
        let rows: Vec<EventRow> = (0..20)
            .map(|i| {
                let kind = if i % 2 == 0 {
                    EventKind::Typed(EventType::Motion)
                } else {
                    EventKind::Typed(EventType::Audio)
                };
                row(i, 1000 - i, kind)
            })
            .collect();
        let (page, total) = page_of(&rows, None, 0, 8);
        assert_eq!(total, 20);
        assert_eq!(page.len(), 8);
        assert_eq!(page[0].id, 0);
        let (page, total) = page_of(&rows, None, 2, 8);
        assert_eq!((page.len(), total), (4, 20));
        let (page, total) = page_of(&rows, Some(EventType::Motion), 1, 8);
        assert_eq!(total, 10);
        assert_eq!(page.len(), 2);
        assert!(
            page.iter()
                .all(|r| r.kind == EventKind::Typed(EventType::Motion))
        );
        // Out-of-range page: empty, not a panic.
        let (page, _) = page_of(&rows, Some(EventType::Tampering), 5, 8);
        assert!(page.is_empty());
    }
}
