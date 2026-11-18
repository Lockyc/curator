//! Model-based sequence test of the pop-out bookkeeping: random runs of pop out, select, rename in
//! place, close, reopen, delete, New Window and redock, each driving the same pure helpers the
//! matching `lib.rs` / `commands.rs` path calls, with the invariants checked after every step.
//!
//! The model owns no label rule: every close, reopen and new window takes a label the generator
//! picked from a small pool, so a label another window just freed comes up often.

use crate::webviews::TabState;
use crate::{
    detach_window_token, opening_tabs, popped_out_of, retitle_origins, settle_detached,
    CuratorDetached,
};
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use shell_core::menu::WindowEntry;
use shell_core::windows::plan::entry_for_title;
use std::collections::{HashMap, HashSet};

const LABELS: [&str; 5] = ["l0", "l1", "l2", "l3", "l4"];
const TITLES: [&str; 5] = ["T0", "T1", "T2", "T3", "T4"];
/// Each window's tabs, as (within-window id, load_on_open); the startup tab is the first
/// load_on_open one.
const TABS: [(&str, bool); 3] = [("tab-0", true), ("tab-1", false), ("tab-2", true)];

#[derive(Clone, Debug)]
enum Op {
    PopOut { win: usize, tab: usize },
    Select { win: usize, tab: usize },
    Rename { win: usize, title: usize },
    Close { win: usize, label: usize },
    Reopen { win: usize, label: usize },
    Delete { win: usize },
    NewWindow { title: usize, label: usize },
    Redock { n: usize },
}

/// The generator: the system under test holds the truth, so the reference only hands out indices
/// that the concrete side resolves (out-of-range or inapplicable steps are no-ops there).
struct Ops;

impl ReferenceStateMachine for Ops {
    type State = ();
    type Transition = Op;

    fn init_state() -> BoxedStrategy<()> {
        Just(()).boxed()
    }

    fn transitions(_: &()) -> BoxedStrategy<Op> {
        let w = 0..LABELS.len();
        let t = 0..TABS.len();
        prop_oneof![
            3 => (w.clone(), t.clone()).prop_map(|(win, tab)| Op::PopOut { win, tab }),
            2 => (w.clone(), t).prop_map(|(win, tab)| Op::Select { win, tab }),
            2 => (w.clone(), 0..TITLES.len()).prop_map(|(win, title)| Op::Rename { win, title }),
            2 => (w.clone(), 0..LABELS.len()).prop_map(|(win, label)| Op::Close { win, label }),
            2 => (w.clone(), 0..LABELS.len()).prop_map(|(win, label)| Op::Reopen { win, label }),
            1 => w.clone().prop_map(|win| Op::Delete { win }),
            1 => (0..TITLES.len(), 0..LABELS.len())
                .prop_map(|(title, label)| Op::NewWindow { title, label }),
            3 => w.prop_map(|n| Op::Redock { n }),
        ]
        .boxed()
    }

    fn apply(state: (), _: &Op) {
        state
    }
}

/// The app's pop-out bookkeeping: the `WindowSet`'s entries, each open window's `TabState`, and
/// `AppState.detached`.
struct Sys {
    entries: Vec<WindowEntry>,
    tabs: HashMap<String, TabState>,
    detached: HashMap<String, CuratorDetached>,
}

fn views(wid: &str) -> Vec<curator_config::TabView> {
    TABS.iter()
        .map(|(id, load_on_open)| curator_config::TabView {
            label: shell_core::label::child_label(wid, id),
            group: None,
            title: id.to_string(),
            url: format!("https://{id}.example"),
            load_on_open: *load_on_open,
            reload_every: None,
            unread: Default::default(),
            session: "default".into(),
        })
        .collect()
}

impl Sys {
    fn label_free(&self, label: &str, except: Option<usize>) -> bool {
        self.entries
            .iter()
            .enumerate()
            .all(|(i, e)| Some(i) == except || e.id != label)
    }

    /// `open_window`: the tabs popped out of this window stay placeholders.
    fn open(&mut self, i: usize) {
        let (wid, title) = (self.entries[i].id.clone(), self.entries[i].title.clone());
        self.entries[i].open = true;
        let out = popped_out_of(&mut self.detached, &wid, &title);
        let v = views(&wid);
        let startup = v.iter().find(|v| v.load_on_open).map(|v| v.label.clone());
        let (tabs, _) = opening_tabs(&v, &out, startup.as_deref());
        self.tabs.insert(wid, tabs);
    }

    /// `window_closed`, after the set listed the window as `entries` now say: settle the pop-outs.
    fn settle(&mut self) {
        for orphan in settle_detached(&mut self.detached, &self.entries) {
            // The destroyed detached window's redock finds no bookkeeping left.
            self.redock(&orphan);
        }
        for d in self.detached.values() {
            assert!(
                entry_for_title(&self.entries, &d.origin_title).is_some(),
                "a pop-out of {:?} survived settling after its window left the config",
                d.origin_title
            );
        }
    }

    /// `pop_out_tab`, including its failure path when the detached window's label is taken.
    fn pop_out(&mut self, i: usize, tab: usize) {
        let e = &self.entries[i];
        let (wid, title) = (e.id.clone(), e.title.trim().to_string());
        let label = shell_core::label::child_label(&wid, TABS[tab].0);
        let rt = self
            .tabs
            .get_mut(&wid)
            .expect("an open window has a runtime");
        if rt.is_detached(&label) {
            return; // the chrome hides the control, and pop_out_tab refuses
        }
        rt.mark_detached(&label);
        let key = shell_core::detach::detached_label(&detach_window_token(&label));
        if self.detached.contains_key(&key) {
            // open_detached fails: Tauri window labels are unique. The tab is restored.
            rt.clear_detached(&label);
            rt.mark_created(&label);
            rt.set_active(&label);
            return;
        }
        self.detached.insert(
            key,
            CuratorDetached {
                origin_wid: wid,
                origin_title: title,
                tab_label: label,
            },
        );
    }

    /// `redock`, when the detached window `key` closed.
    fn redock(&mut self, key: &str) {
        let home = self.detached.get_mut(key).and_then(|d| {
            let home = entry_for_title(&self.entries, &d.origin_title)?;
            d.rehome(&home.id);
            Some(home.id.clone())
        });
        let Some(d) = self.detached.get(key) else {
            return;
        };
        let tab_label = d.tab_label.clone();
        let Some(origin) = home else {
            self.detached.remove(key);
            return;
        };
        let i = self.entries.iter().position(|e| e.id == origin).unwrap();
        if !self.entries[i].open {
            self.open(i);
        }
        self.detached.remove(key);
        let rt = self.tabs.get_mut(&origin).unwrap();
        rt.clear_detached(&tab_label);
        rt.mark_created(&tab_label);
        rt.set_active(&tab_label);
    }
}

struct PopOutModel;

impl StateMachineTest for PopOutModel {
    type SystemUnderTest = Sys;
    type Reference = Ops;

    fn init_test(_: &()) -> Sys {
        let mut sys = Sys {
            entries: (0..2)
                .map(|i| WindowEntry {
                    id: LABELS[i].into(),
                    title: TITLES[i].into(),
                    open: false,
                    colour: None,
                })
                .collect(),
            tabs: HashMap::new(),
            detached: HashMap::new(),
        };
        sys.open(0);
        sys.open(1);
        sys
    }

    fn apply(mut sys: Sys, _: &(), op: Op) -> Sys {
        let n = sys.entries.len();
        let pick = |win: usize| (n > 0).then(|| win % n);
        match op {
            Op::PopOut { win, tab } => {
                if let Some(i) = pick(win).filter(|&i| sys.entries[i].open) {
                    sys.pop_out(i, tab);
                }
            }
            Op::Select { win, tab } => {
                if let Some(i) = pick(win).filter(|&i| sys.entries[i].open) {
                    let wid = sys.entries[i].id.clone();
                    let label = shell_core::label::child_label(&wid, TABS[tab].0);
                    let rt = sys.tabs.get_mut(&wid).unwrap();
                    // A popped-out row raises its window instead (raise_popped_window).
                    if !rt.is_detached(&label) {
                        rt.mark_created(&label);
                        rt.set_active(&label);
                    }
                }
            }
            Op::Rename { win, title } => {
                let new = TITLES[title];
                let taken = sys.entries.iter().any(|e| e.title.trim() == new);
                if let Some(i) = pick(win).filter(|&i| sys.entries[i].open && !taken) {
                    // Edit Window keeps the live label; window_retitled carries the title over.
                    let old = std::mem::replace(&mut sys.entries[i].title, new.into());
                    retitle_origins(&mut sys.detached, &old, new);
                }
            }
            Op::Close { win, label } => {
                if let Some(i) = pick(win).filter(|&i| sys.entries[i].open) {
                    let wid = sys.entries[i].id.clone();
                    sys.tabs.remove(&wid);
                    if sys.label_free(LABELS[label], Some(i)) {
                        sys.entries[i].id = LABELS[label].into();
                    }
                    sys.entries[i].open = false;
                    sys.settle();
                }
            }
            Op::Reopen { win, label } => {
                if let Some(i) = pick(win).filter(|&i| !sys.entries[i].open) {
                    if sys.label_free(LABELS[label], Some(i)) {
                        sys.entries[i].id = LABELS[label].into();
                    }
                    sys.open(i);
                }
            }
            Op::Delete { win } => {
                // Delete Window or a config edit: either way reload_windows settles the pop-outs,
                // and window_closed does too when the window was open.
                if let Some(i) = pick(win) {
                    let e = sys.entries.remove(i);
                    sys.tabs.remove(&e.id);
                    sys.settle();
                }
            }
            Op::NewWindow { title, label } => {
                let taken = sys.entries.iter().any(|e| e.title.trim() == TITLES[title]);
                if !taken && sys.label_free(LABELS[label], None) {
                    sys.entries.push(WindowEntry {
                        id: LABELS[label].into(),
                        title: TITLES[title].into(),
                        open: false,
                        colour: None,
                    });
                    sys.open(sys.entries.len() - 1);
                }
            }
            Op::Redock { n } => {
                let mut keys: Vec<String> = sys.detached.keys().cloned().collect();
                keys.sort();
                if !keys.is_empty() {
                    let key = keys[n % keys.len()].clone();
                    sys.redock(&key);
                }
            }
        }
        sys
    }

    fn check_invariants(sys: &Sys, _: &()) {
        let mut seen = HashSet::new();
        for d in sys.detached.values() {
            assert!(
                d.tab_label.starts_with(&format!("{}:", d.origin_wid)),
                "{} is not namespaced under its origin {}",
                d.tab_label,
                d.origin_wid
            );
            assert!(
                seen.insert(d.tab_label.clone()),
                "two pop-outs share the tab {}",
                d.tab_label
            );
            if let Some(home) = entry_for_title(&sys.entries, &d.origin_title) {
                assert_eq!(home.id, d.origin_wid, "a pop-out points off its origin");
                if home.open {
                    let rt = &sys.tabs[&home.id];
                    assert!(
                        rt.is_detached(&d.tab_label) && !rt.is_created(&d.tab_label),
                        "the open origin {} shows popped-out {} as an ordinary tab",
                        home.id,
                        d.tab_label
                    );
                }
            }
        }
        for (wid, rt) in &sys.tabs {
            for v in views(wid) {
                assert!(
                    !(rt.is_created(&v.label) && rt.is_detached(&v.label)),
                    "{} is both loaded and popped out",
                    v.label
                );
            }
            if let Some(a) = rt.active() {
                assert!(!rt.is_detached(a), "popped-out {a} is the active tab");
            }
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn pop_out_bookkeeping_survives_any_window_sequence(sequential 1..40 => PopOutModel);
}
