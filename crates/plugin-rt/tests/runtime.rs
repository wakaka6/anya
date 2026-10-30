//! What the app sees of the runtime: what is served while a plugin runs, the
//! order things stop in, a plugin that fails, and why a plugin is on.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anya_plugin_rt::{Active, ApplyError, Error, Plugin, Point, Runtime, State, Why, compose};

#[derive(Default)]
struct Points {
    names: Point<str>,
}

type Log = Arc<Mutex<Vec<String>>>;

/// Records in `log` when it is dropped.
struct Note(Log, String);

impl Drop for Note {
    fn drop(&mut self) {
        self.0.lock().expect("log").push(self.1.clone());
    }
}

struct Named {
    id: &'static str,
    serves: &'static str,
    fails: bool,
    log: Log,
}

impl Plugin<Points> for Named {
    fn id(&self) -> &str {
        self.id
    }

    fn apply(&self, points: &Points) -> Result<Active, ApplyError> {
        if self.fails {
            return Err(ApplyError(format!("{} cannot start", self.id)));
        }
        let served = points.names.serve(Arc::from(self.serves));
        Ok(Active::new()
            .hold(Note(self.log.clone(), format!("{} first", self.id)))
            .hold(served)
            .hold(Note(self.log.clone(), format!("{} last", self.id))))
    }
}

fn plugin(id: &'static str, log: &Log) -> Box<dyn Plugin<Points>> {
    Box::new(Named {
        id,
        serves: id,
        fails: false,
        log: log.clone(),
    })
}

fn served(runtime: &Runtime<Points>) -> Vec<String> {
    runtime
        .points()
        .names
        .all()
        .iter()
        .map(|n| n.to_string())
        .collect()
}

fn only<'a>(ids: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |id| ids.contains(&id)
}

#[test]
fn a_plugin_serves_only_while_it_is_wanted() {
    let log = Log::default();
    let mut runtime = Runtime::new(
        Points::default(),
        vec![plugin("a", &log), plugin("b", &log), plugin("c", &log)],
    )
    .expect("runtime");
    assert!(served(&runtime).is_empty(), "nothing runs before reconcile");

    runtime.reconcile(only(&["a", "c"]));
    assert_eq!(served(&runtime), ["a", "c"]);

    runtime.reconcile(only(&["a", "b", "c"]));
    assert_eq!(
        served(&runtime),
        ["a", "c", "b"],
        "b starts after the others"
    );

    runtime.reconcile(only(&["b"]));
    assert_eq!(served(&runtime), ["b"]);
    let states: Vec<_> = runtime.states().into_iter().map(|(_, s)| s).collect();
    assert_eq!(states, [State::Off, State::Active, State::Off]);
}

#[test]
fn what_a_plugin_holds_is_released_newest_first_and_plugins_newest_first() {
    let log = Log::default();
    let mut runtime = Runtime::new(
        Points::default(),
        vec![plugin("a", &log), plugin("b", &log)],
    )
    .expect("runtime");
    runtime.reconcile(only(&["a", "b"]));
    runtime.reconcile(only(&[]));
    assert_eq!(
        *log.lock().expect("log"),
        ["b last", "b first", "a last", "a first"]
    );

    log.lock().expect("log").clear();
    runtime.reconcile(only(&["a", "b"]));
    drop(runtime);
    assert_eq!(
        *log.lock().expect("log"),
        ["b last", "b first", "a last", "a first"],
        "dropping the runtime stops plugins the same way"
    );
}

#[test]
fn a_failing_plugin_is_reported_and_tried_again_without_stopping_the_others() {
    let log = Log::default();
    let broken = Box::new(Named {
        id: "broken",
        serves: "broken",
        fails: true,
        log: log.clone(),
    });
    let mut runtime = Runtime::new(
        Points::default(),
        vec![plugin("a", &log), broken, plugin("c", &log)],
    )
    .expect("runtime");

    runtime.reconcile(|_| true);
    assert_eq!(served(&runtime), ["a", "c"]);
    assert_eq!(
        runtime
            .states()
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<_>>(),
        [
            State::Active,
            State::Failed("broken cannot start".to_owned()),
            State::Active
        ]
    );

    runtime.reconcile(only(&["a", "c"]));
    assert_eq!(
        runtime.states()[1].1,
        State::Off,
        "no longer wanted is not failed"
    );
}

#[test]
fn a_registration_withdraws_only_its_own_item() {
    let point: Point<str> = Point::new();
    let first = point.serve(Arc::from("same"));
    let second = point.serve(Arc::from("same"));
    assert_eq!(point.all().len(), 2);
    drop(first);
    assert_eq!(point.all().len(), 1);
    drop(second);
    assert!(point.all().is_empty());
}

#[test]
fn ids_must_be_well_formed_and_unique() {
    let log = Log::default();
    assert!(matches!(
        Runtime::new(Points::default(), vec![plugin("a", &log), plugin("a", &log)]),
        Err(Error::DuplicateId(id)) if id == "a"
    ));
    assert!(matches!(
        Runtime::new(Points::default(), vec![plugin("Not Ok", &log)]),
        Err(Error::BadId(_))
    ));
}

#[test]
fn a_plugin_is_on_unless_the_user_turned_it_off_and_stays_on_while_in_use() {
    let rows = BTreeMap::from([
        ("a".to_owned(), false),
        ("b".to_owned(), false),
        ("c".to_owned(), true),
    ]);
    let composed = compose(["a", "b", "c", "d"], &rows, |id| id == "b");
    let summary: Vec<_> = composed
        .iter()
        .map(|c| (c.id.as_str(), c.on, c.why))
        .collect();
    assert_eq!(
        summary,
        [
            ("a", false, Why::User),
            ("b", true, Why::InUse),
            ("c", true, Why::User),
            ("d", true, Why::Default),
        ]
    );
}
