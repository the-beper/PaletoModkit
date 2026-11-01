//! The "Hash generator" window: it works with no game at all, gives the same
//! values the core functions do, and the names saved to the store resolve.

use std::time::Duration;

use eframe::egui;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::App;

/// The three text boxes of the window (generate, resolve, paste), in their
/// order on screen.
fn edit_boxes<'t>(harness: &'t Harness<'t, App>) -> Vec<egui_kittest::Node<'t>> {
    let mut nodes: Vec<egui_kittest::Node<'_>> = harness.query_all_by_role(Role::MultilineTextInput).collect();
    nodes.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()).then_with(|| a.rect().left().total_cmp(&b.rect().left())));
    nodes
}

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..300 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(2);
}

#[test]
fn generate_resolve_and_import_without_a_game() {
    let dir = tempfile::tempdir().unwrap();
    // No game, and no settings, keys or names store of the user's: a scratch
    // home and a scratch data dir, like the CLI tests do.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::remove_var("PALETO_GAME");
    }

    let mut harness = Harness::builder().with_size([1200.0, 800.0]).build_eframe(|cc| App::new(cc));
    // Tools > Hash generator…, with no game chosen at all.
    harness.get_by_label("Tools").click();
    harness.run_steps(2);
    harness.get_by_label("Hash generator…").click();
    // The window may take a frame to reach the accesskit tree.
    for _ in 0..100 {
        if edit_boxes(&harness).len() == 3 {
            break;
        }
        harness.step();
    }
    let boxes = edit_boxes(&harness).len();
    assert_eq!(boxes, 3, "the generate, resolve and paste boxes of the window");
    harness.run_steps(1);

    // Hash a name: the same values the core functions give, hexadecimal,
    // unsigned and signed decimal (each cell is a button that copies it).
    edit_boxes(&harness)[0].click();
    harness.run_steps(2);
    edit_boxes(&harness)[0].type_text("Adder_vehicle");
    harness.run_steps(2);
    let hash = paleto_core::hashes::Mode::Lower.hash("Adder_vehicle");
    harness.get_by_label(&paleto_core::hashes::as_hex(hash));
    harness.get_by_label(&paleto_core::hashes::as_unsigned(hash));
    harness.get_by_label(&paleto_core::hashes::as_signed(hash));

    // Another mode: the rows are said again in the way the mode hashes.
    harness.get_by_label("exact bytes (metadata names: CMapData)").click();
    harness.run_steps(2);
    let exact = paleto_core::hashes::Mode::Exact.hash("Adder_vehicle");
    assert_ne!(exact, hash, "the case matters in the exact mode");
    harness.get_by_label(&paleto_core::hashes::as_hex(exact));
    // ...and back to the default mode.
    harness.get_by_label("lowercase (as the game hashes asset names)").click();
    harness.run_steps(2);
    harness.get_by_label(&paleto_core::hashes::as_hex(hash));

    // Resolve values: a metadata name from the built-in dictionary, an unknown
    // one, and text of no form — plus two values that only the names to save
    // will resolve ("My_adder" at the hash of its own text, and the two names
    // one hash holds at once: "puwaaa"/"thaeaa" collide). The dictionaries are
    // not inverted: the tool only looks up.
    edit_boxes(&harness)[1].click();
    harness.run_steps(2);
    edit_boxes(&harness)[1].type_text(&format!(
        "3545841574\n0xB779A091\nnot_a_value\n{:08x}\n{:08x}\n",
        paleto_core::hashes::Mode::Lower.hash("My_adder"),
        paleto_core::hashes::Mode::Lower.hash("puwaaa")
    ));
    harness.run_steps(2);
    // Three rows are unknown at this point: the saved names do not resolve yet.
    assert_eq!(harness.get_all_by_label("unknown").count(), 3);
    harness.get_by_label("CMapData");
    harness.get_by_label("not a value");

    // Save names: the store is empty, so all three are new.
    edit_boxes(&harness)[2].click();
    harness.run_steps(2);
    edit_boxes(&harness)[2].type_text("My_adder\nPuwaaa\nThaeaa\n");
    harness.run_steps(2);
    harness.get_by_label("Save these names…").click_accesskit();
    settle(&mut harness);
    // The names just saved now resolve the rows typed above in place.
    harness.get_by_label("3 names saved.");
    harness.get_by_label("The resolution also knows 3 names of yours.");
    harness.get_by_label("My_adder");
    harness.get_by_label("Puwaaa, Thaeaa");
    // Only the vehicle hash stays unknown.
    assert_eq!(harness.get_all_by_label("unknown").count(), 1);

    // Closed by the close button, and everything of it with the window.
    harness.get_by_label("✖").click();
    harness.run_steps(2);
    assert_eq!(edit_boxes(&harness).len(), 0);
}
