//! The panic policy of [development.md](../../docs/development.md): the
//! production code of this crate keeps a count within the cap, and holds no
//! `panic!` of its own. Test code is exempt.

#[test]
fn the_panic_policy_cap_holds() {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut sites = 0;
    let mut panics = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.to_string_lossy().into_owned();
            if name.ends_with("tests.rs") || name.ends_with("fixtures.rs") {
                continue;
            }
            // Everything before the first `#[cfg(test)]` is production code.
            let text = std::fs::read_to_string(&path).unwrap();
            let production = text.split("#[cfg(test)]").next().unwrap_or_default();
            sites += production.matches(".unwrap()").count() + production.matches("expect(").count();
            panics += production.matches("panic!(").count();
        }
    }
    assert!(
        sites <= 32,
        "{sites} `.unwrap()`/`expect(` sites in the production code are over the cap of 32 (29 when the policy landed): \
         classify every new site by [the policy](../../docs/development.md), and raise the cap with a note in docs/implementation-status.md"
    );
    assert_eq!(0, panics, "{panics} `panic!(` calls in the production code: the policy has them nowhere");
}
