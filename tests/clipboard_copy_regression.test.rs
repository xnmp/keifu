//! Regression coverage for clipboard-command fallback from issue #192.

mod common;

#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use keifu::{
        action::Action,
        app::{App, AppMode, CommitMenuItem},
        toast::ToastKind,
    };

    use super::common::{commit_file, init_repo, Seed};

    struct EnvRestore {
        name: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = env::var_os(name);
            // This integration-test binary has one test, so its temporary
            // process environment cannot race another test in the same process.
            env::set_var(name, value);
            Self { name, original }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => env::set_var(self.name, value),
                None => env::remove_var(self.name),
            }
        }
    }

    fn write_command(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn select_copy_hash(app: &mut App) {
        app.handle_action(Action::FocusGraph).unwrap();
        app.handle_action(Action::OpenCommitMenu).unwrap();
        let copy_hash_index = match &app.mode {
            AppMode::CommitMenu { items, .. } => items
                .iter()
                .position(|item| *item == CommitMenuItem::CopyHash)
                .expect("commit menu includes Copy commit hash"),
            mode => panic!("expected commit menu, got {mode:?}"),
        };
        for _ in 0..copy_hash_index {
            app.handle_action(Action::MoveDown).unwrap();
        }
        app.handle_action(Action::MenuSelect).unwrap();
    }

    #[test]
    fn failed_xclip_falls_through_to_wl_copy_for_selected_commit_hash() {
        let (tempdir, repo) = init_repo(Seed::Empty);
        commit_file(repo.repo(), "base.txt", "base", "base commit");
        let commit = commit_file(repo.repo(), "commit.txt", "contents", "clipboard target");
        let commands_dir = tempdir.path().join("clipboard-commands");
        fs::create_dir(&commands_dir).unwrap();
        let copied_hash = tempdir.path().join("copied-hash");

        write_command(&commands_dir.join("xclip"), "#!/bin/sh\nexit 1\n");
        write_command(
            &commands_dir.join("wl-copy"),
            "#!/bin/sh\n/bin/cat > \"$KEIFU_TEST_CLIPBOARD_OUTPUT\"\n",
        );

        let _path = EnvRestore::set("PATH", PathBuf::from(&commands_dir));
        let _output = EnvRestore::set("KEIFU_TEST_CLIPBOARD_OUTPUT", &copied_hash);
        let mut app = App::from_repo(repo).unwrap();
        let commit_node = app
            .graph_layout
            .nodes
            .iter()
            .position(|node| node.commit.as_ref().is_some_and(|node| node.oid == commit))
            .expect("the committed hash is visible in the graph");
        app.graph_nav.graph_list_state.select(Some(commit_node));

        select_copy_hash(&mut app);

        assert_eq!(
            fs::read_to_string(&copied_hash).unwrap(),
            commit.to_string(),
            "the succeeding clipboard command receives the selected commit hash"
        );
        assert!(
            app.toasts.visible().iter().any(|toast| {
                toast.kind == ToastKind::Success
                    && toast.text.starts_with("Copied ")
                    && !toast.text.contains("via OSC 52")
            }),
            "successful fallback command produces the normal copy-success toast"
        );
    }
}
