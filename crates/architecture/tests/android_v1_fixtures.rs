//! Integration test for Wave 3 of the AST-rubric migration.
//!
//! Mirrors `spring_v8_fixtures.rs` but for the Android v1 rubric in
//! `config/android-rubric.md`. Each test lays down a small Java fixture
//! under a tempdir, runs `scan_repo_to_db` against the production
//! `config/architecture.toml`, and asserts whether the rule fired (BAD)
//! or stayed silent (GOOD).

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use sprint_grader_architecture::{scan_repo_to_db, ArchitectureRules};
use tempfile::TempDir;

fn write_java(repo_root: &Path, rel: &str, body: &str) {
    let p = repo_root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn git_init(dir: &Path) {
    let run = |args: &[&str]| {
        let s = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git invocation");
        assert!(s.success(), "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "fixtures@example.com"]);
    run(&["config", "user.name", "Fixtures"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "fixture"]);
}

fn scan_with_production_config(repo: &Path) -> HashSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/architecture.toml");
    let rules = ArchitectureRules::load(&path).expect("config/architecture.toml must parse");
    let conn = Connection::open_in_memory().unwrap();
    sprint_grader_core::db::apply_schema(&conn).unwrap();
    scan_repo_to_db(&conn, repo, "udg/fixture", &rules).unwrap();
    let mut out = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT rule_name FROM architecture_violations WHERE repo_full_name = ?")
        .unwrap();
    let rows = stmt
        .query_map(["udg/fixture"], |r| r.get::<_, String>(0))
        .unwrap();
    for r in rows {
        out.insert(r.unwrap());
    }
    out
}

// ---------- VIEWMODEL_IMPORTS_ANDROID_UI ----------

#[test]
fn bad_case_viewmodel_imports_android_ui_widget() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import android.widget.TextView;\n\
         import androidx.lifecycle.ViewModel;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_IMPORTS_ANDROID_UI"),
        "expected VIEWMODEL_IMPORTS_ANDROID_UI, got: {hits:?}"
    );
}

#[test]
fn bad_case_viewmodel_imports_binding() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import com.x.databinding.FragmentHomeBinding;\n\
         import androidx.lifecycle.ViewModel;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_IMPORTS_ANDROID_UI"),
        "expected VIEWMODEL_IMPORTS_ANDROID_UI for binding import, got: {hits:?}"
    );
}

#[test]
fn good_case_viewmodel_imports_only_mvvm_types() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import androidx.lifecycle.MutableLiveData;\n\
         import androidx.lifecycle.ViewModel;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("VIEWMODEL_IMPORTS_ANDROID_UI"),
        "clean ViewModel imports must not fire: {hits:?}"
    );
}

#[test]
fn bad_case_viewmodel_imports_project_fragment_class() {
    // The import regex catches any imported class whose name contains
    // "Fragment" — not just the literal `androidx.fragment.app.Fragment`.
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import com.x.ui.home.HomeFragment;\n\
         import androidx.lifecycle.ViewModel;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_IMPORTS_ANDROID_UI"),
        "project Fragment subclass import must fire, got: {hits:?}"
    );
}

#[test]
fn bad_case_viewmodel_imports_project_activity_class() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import com.x.ui.main.MainActivity;\n\
         import androidx.lifecycle.ViewModel;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_IMPORTS_ANDROID_UI"),
        "project Activity subclass import must fire, got: {hits:?}"
    );
}

// ---------- VIEWMODEL_HOLDS_CONTEXT ----------

#[test]
fn bad_case_viewmodel_holds_context() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             private Context context;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_HOLDS_CONTEXT"),
        "expected VIEWMODEL_HOLDS_CONTEXT, got: {hits:?}"
    );
}

#[test]
fn good_case_android_view_model_exempt() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends AndroidViewModel {\n\
             private Context context;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("VIEWMODEL_HOLDS_CONTEXT"),
        "AndroidViewModel is exempt by not_extends: {hits:?}"
    );
}

// ---------- FRAGMENT_BYPASSES_VIEWMODEL ----------

#[test]
fn bad_case_fragment_holds_repository_field() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             private HomeRepository repository;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FRAGMENT_BYPASSES_VIEWMODEL"),
        "expected FRAGMENT_BYPASSES_VIEWMODEL, got: {hits:?}"
    );
}

#[test]
fn bad_case_activity_imports_retrofit() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeActivity.java",
        "package com.x.home;\n\
         import retrofit2.Retrofit;\n\
         public class HomeActivity extends AppCompatActivity {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FRAGMENT_BYPASSES_VIEWMODEL"),
        "expected FRAGMENT_BYPASSES_VIEWMODEL via retrofit import, got: {hits:?}"
    );
}

// ---------- REPOSITORY_DEPENDS_ON_VIEW_LAYER ----------

#[test]
fn bad_case_repository_imports_fragment() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/data/UserRepository.java",
        "package com.x.data;\n\
         import androidx.fragment.app.Fragment;\n\
         public class UserRepository {\n\
             void notify(Fragment f) {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("REPOSITORY_DEPENDS_ON_VIEW_LAYER"),
        "expected REPOSITORY_DEPENDS_ON_VIEW_LAYER, got: {hits:?}"
    );
}

#[test]
fn good_case_repository_with_application_context_is_silent() {
    // Plain `android.content.Context` import — the rubric's allowlist
    // item 9 explicitly permits this for `@ApplicationContext Context`.
    // Our forbidden_import regex omits `android.content.Context` from
    // the trigger list, so this fixture must not fire.
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/data/UserRepository.java",
        "package com.x.data;\n\
         import android.content.Context;\n\
         public class UserRepository {\n\
             public UserRepository(Context context) {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("REPOSITORY_DEPENDS_ON_VIEW_LAYER"),
        "@ApplicationContext Context must not fire: {hits:?}"
    );
}

// ---------- ASYNCTASK_USAGE ----------

#[test]
fn bad_case_asynctask_import_fires() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/util/LoadUsers.java",
        "package com.x.util;\n\
         import android.os.AsyncTask;\n\
         class LoadUsers extends AsyncTask {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("ASYNCTASK_USAGE"),
        "expected ASYNCTASK_USAGE, got: {hits:?}"
    );
}

#[test]
fn good_case_executors_is_silent() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/util/IoExecutor.java",
        "package com.x.util;\n\
         import java.util.concurrent.Executors;\n\
         public class IoExecutor {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("ASYNCTASK_USAGE"),
        "Executors import must not fire: {hits:?}"
    );
}

// ---------- STATIC_VIEW_OR_CONTEXT_FIELD ----------

#[test]
fn bad_case_static_activity_field_fires() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/util/Holder.java",
        "package com.x.util;\n\
         public class Holder {\n\
             private static Activity sActivity;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("STATIC_VIEW_OR_CONTEXT_FIELD"),
        "expected STATIC_VIEW_OR_CONTEXT_FIELD, got: {hits:?}"
    );
}

#[test]
fn good_case_static_string_constant_is_silent() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/util/Constants.java",
        "package com.x.util;\n\
         public class Constants {\n\
             public static final String BASE_URL = \"https://api.example.com\";\n\
             public static final int TIMEOUT = 30;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("STATIC_VIEW_OR_CONTEXT_FIELD"),
        "stdlib constants must not fire: {hits:?}"
    );
}

// ---------- FRAGMENT_BINDING_NOT_NULLED ----------

#[test]
fn bad_case_fragment_binding_no_on_destroy_view() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             private FragmentHomeBinding binding;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FRAGMENT_BINDING_NOT_NULLED"),
        "expected FRAGMENT_BINDING_NOT_NULLED, got: {hits:?}"
    );
}

#[test]
fn good_case_fragment_binding_nulled_in_on_destroy_view() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             private FragmentHomeBinding binding;\n\
             @Override public void onDestroyView() {\n\
                 super.onDestroyView();\n\
                 binding = null;\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("FRAGMENT_BINDING_NOT_NULLED"),
        "binding=null in onDestroyView is the fix: {hits:?}"
    );
}

// ---------- LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS ----------

#[test]
fn bad_case_observe_this_in_fragment() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void onResume() {\n\
                 viewModel.getUsers().observe(this, x -> render(x));\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS"),
        "expected LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS, got: {hits:?}"
    );
}

#[test]
fn good_case_observe_with_view_lifecycle_owner() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void onResume() {\n\
                 viewModel.getUsers().observe(getViewLifecycleOwner(), x -> render(x));\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS"),
        "getViewLifecycleOwner() is correct: {hits:?}"
    );
}

// ---------- VIEWMODEL_BYPASSES_REPOSITORY ----------

#[test]
fn bad_case_viewmodel_holds_apiservice() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             private UserApiService api;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_BYPASSES_REPOSITORY"),
        "expected VIEWMODEL_BYPASSES_REPOSITORY, got: {hits:?}"
    );
}

#[test]
fn bad_case_viewmodel_imports_retrofit() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import retrofit2.Retrofit;\n\
         public class HomeViewModel extends ViewModel {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("VIEWMODEL_BYPASSES_REPOSITORY"),
        "expected VIEWMODEL_BYPASSES_REPOSITORY via retrofit import, got: {hits:?}"
    );
}

// ---------- FINDVIEWBYID_USAGE ----------

#[test]
fn bad_case_fragment_findviewbyid_fires() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void onViewCreated(View view) {\n\
                 TextView title = view.findViewById(1);\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FINDVIEWBYID_USAGE"),
        "expected FINDVIEWBYID_USAGE, got: {hits:?}"
    );
}

#[test]
fn bad_case_activity_findviewbyid_bare_fires() {
    // Activity inherits `findViewById` so the bare form (no receiver) is
    // legal Java but still bypasses ViewBinding. The call_regex
    // `(^|\.)findViewById$` catches both forms.
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeActivity.java",
        "package com.x.home;\n\
         public class HomeActivity extends AppCompatActivity {\n\
             void onCreate() {\n\
                 TextView t = findViewById(1);\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FINDVIEWBYID_USAGE"),
        "bare findViewById in Activity must fire, got: {hits:?}"
    );
}

#[test]
fn bad_case_activity_findviewbyid_dotted_fires() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeActivity.java",
        "package com.x.home;\n\
         public class HomeActivity extends AppCompatActivity {\n\
             void onCreate() {\n\
                 TextView t = this.findViewById(1);\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FINDVIEWBYID_USAGE"),
        "dotted findViewById in Activity must fire, got: {hits:?}"
    );
}

// ---------- NAVIGATION_VIA_FRAGMENT_TRANSACTION ----------

#[test]
fn bad_case_navigation_via_fragment_transaction() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void go() {\n\
                 getChildFragmentManager().beginTransaction().replace(1, new DetailFragment()).commit();\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("NAVIGATION_VIA_FRAGMENT_TRANSACTION"),
        "expected NAVIGATION_VIA_FRAGMENT_TRANSACTION, got: {hits:?}"
    );
}

// ---------- FRAGMENT_CASTS_PARENT_ACTIVITY ----------

#[test]
fn bad_case_fragment_casts_parent_activity() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void notifyHost() {\n\
                 ((MainActivity) requireActivity()).showProgressBar();\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FRAGMENT_CASTS_PARENT_ACTIVITY"),
        "expected FRAGMENT_CASTS_PARENT_ACTIVITY, got: {hits:?}"
    );
}

// ---------- RAW_THREAD_FOR_BACKGROUND_WORK ----------

#[test]
fn bad_case_new_thread_in_fragment() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             void load() {\n\
                 new Thread(() -> repository.fetch()).start();\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("RAW_THREAD_FOR_BACKGROUND_WORK"),
        "expected RAW_THREAD_FOR_BACKGROUND_WORK, got: {hits:?}"
    );
}

#[test]
fn bad_case_new_thread_in_repository() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/data/UserRepository.java",
        "package com.x.data;\n\
         public class UserRepository {\n\
             void fetch() {\n\
                 new Thread(() -> {}).start();\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("RAW_THREAD_FOR_BACKGROUND_WORK"),
        "expected RAW_THREAD_FOR_BACKGROUND_WORK in repository, got: {hits:?}"
    );
}

// ---------- MUTABLELIVEDATA_EXPOSED_PUBLICLY ----------

#[test]
fn bad_case_public_mutable_live_data_field() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             public MutableLiveData users;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("MUTABLELIVEDATA_EXPOSED_PUBLICLY"),
        "expected MUTABLELIVEDATA_EXPOSED_PUBLICLY, got: {hits:?}"
    );
}

#[test]
fn bad_case_public_mutable_live_data_getter() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             public MutableLiveData getUsers() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("MUTABLELIVEDATA_EXPOSED_PUBLICLY"),
        "expected MUTABLELIVEDATA_EXPOSED_PUBLICLY for getter, got: {hits:?}"
    );
}

#[test]
fn good_case_private_mutable_live_data_with_live_data_getter() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             private final MutableLiveData users = new MutableLiveData();\n\
             public LiveData getUsers() { return users; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("MUTABLELIVEDATA_EXPOSED_PUBLICLY"),
        "private MutableLiveData + LiveData getter must not fire: {hits:?}"
    );
}

// ---------- FAT_FRAGMENT_OR_ACTIVITY_METHOD ----------

#[test]
fn bad_case_fat_fragment_method() {
    let body: String = (1..=45)
        .map(|i| format!("        int v{i} = {i};\n"))
        .collect();
    let src = format!(
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {{\n\
             public void onViewCreated() {{\n\
{body}\
             }}\n\
         }}\n"
    );
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        &src,
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FAT_FRAGMENT_OR_ACTIVITY_METHOD"),
        "expected FAT_FRAGMENT_OR_ACTIVITY_METHOD, got: {hits:?}"
    );
}

// ---------- MISSING_HILT_VIEWMODEL ----------

#[test]
fn bad_case_missing_hilt_viewmodel() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         public class HomeViewModel extends ViewModel {\n\
             @Inject HomeViewModel(UserRepository r) {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("MISSING_HILT_VIEWMODEL"),
        "expected MISSING_HILT_VIEWMODEL, got: {hits:?}"
    );
}

#[test]
fn good_case_hilt_viewmodel_present() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         @HiltViewModel\n\
         public class HomeViewModel extends ViewModel {\n\
             @Inject HomeViewModel(UserRepository r) {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("MISSING_HILT_VIEWMODEL"),
        "@HiltViewModel present → no fire: {hits:?}"
    );
}

// ---------- multi-rule GOOD aggregate ----------

/// One GOOD fixture per rule, all stuffed into one project — together
/// they must produce zero Android v1 violations. Same idea as the Spring
/// equivalent: if any rule mistakenly fires on a clean reference
/// project, this guard catches it.
#[test]
fn all_good_cases_produce_zero_android_v1_violations() {
    let tmp = TempDir::new().unwrap();
    let v1: HashSet<&str> = [
        "VIEWMODEL_IMPORTS_ANDROID_UI",
        "VIEWMODEL_HOLDS_CONTEXT",
        "FRAGMENT_BYPASSES_VIEWMODEL",
        "REPOSITORY_DEPENDS_ON_VIEW_LAYER",
        "ASYNCTASK_USAGE",
        "STATIC_VIEW_OR_CONTEXT_FIELD",
        "FRAGMENT_BINDING_NOT_NULLED",
        "LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS",
        "VIEWMODEL_BYPASSES_REPOSITORY",
        "FINDVIEWBYID_USAGE",
        "NAVIGATION_VIA_FRAGMENT_TRANSACTION",
        "FRAGMENT_CASTS_PARENT_ACTIVITY",
        "RAW_THREAD_FOR_BACKGROUND_WORK",
        "MUTABLELIVEDATA_EXPOSED_PUBLICLY",
        "FAT_FRAGMENT_OR_ACTIVITY_METHOD",
        "MISSING_HILT_VIEWMODEL",
    ]
    .into_iter()
    .collect();

    // Clean ViewModel: HiltViewModel + AndroidX MVVM imports + private
    // MutableLiveData + LiveData getter.
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeViewModel.java",
        "package com.x.home;\n\
         import androidx.lifecycle.LiveData;\n\
         import androidx.lifecycle.MutableLiveData;\n\
         import androidx.lifecycle.ViewModel;\n\
         @HiltViewModel\n\
         public class HomeViewModel extends ViewModel {\n\
             private final MutableLiveData users = new MutableLiveData();\n\
             @Inject HomeViewModel(UserRepository r) {}\n\
             public LiveData getUsers() { return users; }\n\
         }\n",
    );

    // Clean Fragment: binding nulled, observe(getViewLifecycleOwner),
    // no Repository field, no findViewById, no beginTransaction.
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/home/HomeFragment.java",
        "package com.x.home;\n\
         public class HomeFragment extends Fragment {\n\
             private FragmentHomeBinding binding;\n\
             private HomeViewModel viewModel;\n\
             public void onViewCreated(View v) {\n\
                 viewModel.getUsers().observe(getViewLifecycleOwner(), x -> {});\n\
             }\n\
             @Override public void onDestroyView() {\n\
                 super.onDestroyView();\n\
                 binding = null;\n\
             }\n\
         }\n",
    );

    // Clean Repository: only android.content.Context import.
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/data/UserRepository.java",
        "package com.x.data;\n\
         import android.content.Context;\n\
         public class UserRepository {\n\
             public UserRepository(Context ctx) {}\n\
         }\n",
    );

    // Constants — primitive / String statics only.
    write_java(
        tmp.path(),
        "app/src/main/java/com/x/util/Constants.java",
        "package com.x.util;\n\
         public class Constants {\n\
             public static final String BASE_URL = \"https://x\";\n\
         }\n",
    );

    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    let v1_hits: Vec<_> = hits.iter().filter(|h| v1.contains(h.as_str())).collect();
    assert!(
        v1_hits.is_empty(),
        "GOOD fixtures must produce zero Android v1 violations, got: {v1_hits:?}"
    );
}
