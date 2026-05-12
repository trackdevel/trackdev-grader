---
rubric_version: 2
target_model: none
target_stack: android-java-21 / mvvm-with-repository / hilt / retrofit-3 / view-binding / navigation-component
scope: single-file analysis
---

> **TARGETING.** This file is the human-readable spec for the AST rules
> in `config/architecture.toml`. As of Wave 4 of the AST-rubric
> migration it is **no longer fed to an LLM**. The deterministic AST
> engine in `crates/architecture/src/ast_rules.rs` is authoritative;
> this document is reference material for the instructor, and the
> golden source for the `crates/architecture/tests/android_v1_fixtures.rs`
> integration tests. Bump `rubric_version` and tune the corresponding
> `[[ast_rule]]` block in `architecture.toml` when the policy changes.

# Android Architecture Rubric

## Task

You are reviewing ONE Java source file from an Android project for architectural violations. The project's stack is fixed:

- Java 21, Android (one Navigation Component host activity with N fragments; a few extra activities such as Login / Register are allowed)
- MVVM with a Repository layer between ViewModel and data sources
- Retrofit 3 for HTTP
- Hilt for dependency injection
- ViewBinding for view access (no `findViewById`)
- Executors + LiveData for background work (Kotlin coroutines are not available)
- Bottom navigation and/or navigation drawer

You do not see other files. Do not infer cross-file relationships beyond what the imports show.

## Class identification by name suffix

Throughout the rules below, "this file is a Fragment / Activity / ViewModel / Repository" means the file declares a class whose name ends in that suffix **or** whose declared `extends` clause names a class ending in that suffix. Concretely:

- **Fragment** = class name ends in `Fragment`, OR `extends Fragment` / `DialogFragment` / `BottomSheetDialogFragment` / `PreferenceFragmentCompat` (any name from `androidx.fragment.app` or `com.google.android.material.bottomsheet`).
- **Activity** = class name ends in `Activity`, OR `extends Activity` / `AppCompatActivity` / `FragmentActivity` / `ComponentActivity`.
- **ViewModel** = class name ends in `ViewModel`, OR `extends ViewModel` / `AndroidViewModel`, OR annotated `@HiltViewModel`.
- **Repository** = class name ends in `Repository`.
- **ViewBinding type** = type name ends in `Binding` and is referenced as a value (field, local, parameter) — not as an interface or annotation.

## Output format

Emit ONLY this JSON. No prose before or after. No markdown fences around the JSON.

```json
{
  "violations": [
    {
      "rule_id": "<from RULE_IDS table>",
      "severity": "<exact value from RULE_IDS table for that rule_id>",
      "start_line": 12,
      "end_line": 14,
      "explanation": "<≤ 20 words, concrete>"
    }
  ]
}
```

If you find no violations, emit exactly: `{"violations": []}`

Line numbers are 1-indexed and refer to lines of the file as given to you. Both `start_line` and `end_line` are inclusive.

## RULE_IDS (closed enum — fixed severity)

You may emit ONLY these `rule_id` values. The `severity` is fixed by the `rule_id`; emit the severity in this table verbatim.

| rule_id | severity |
|---|---|
| `VIEWMODEL_IMPORTS_ANDROID_UI` | CRITICAL |
| `VIEWMODEL_HOLDS_CONTEXT` | CRITICAL |
| `FRAGMENT_BYPASSES_VIEWMODEL` | CRITICAL |
| `REPOSITORY_DEPENDS_ON_VIEW_LAYER` | CRITICAL |
| `ASYNCTASK_USAGE` | CRITICAL |
| `STATIC_VIEW_OR_CONTEXT_FIELD` | CRITICAL |
| `FRAGMENT_BINDING_NOT_NULLED` | CRITICAL |
| `LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS` | CRITICAL |
| `VIEWMODEL_BYPASSES_REPOSITORY` | WARNING |
| `FINDVIEWBYID_USAGE` | WARNING |
| `NAVIGATION_VIA_FRAGMENT_TRANSACTION` | WARNING |
| `FRAGMENT_CASTS_PARENT_ACTIVITY` | WARNING |
| `RAW_THREAD_FOR_BACKGROUND_WORK` | WARNING |
| `MUTABLELIVEDATA_EXPOSED_PUBLICLY` | WARNING |
| `FAT_FRAGMENT_OR_ACTIVITY_METHOD` | WARNING |
| `MISSING_HILT_VIEWMODEL` | WARNING |

If a candidate finding does not fit one of these `rule_id` values, **do not emit it**. Do not invent new `rule_id` values. Do not rephrase a `rule_id` under a different name.

## Always allowed (never emit a violation for any of these)

These constructs are part of the expected architecture. Emit no violation that references any of them under any `rule_id`. If your candidate finding matches one of these, drop it before output.

1. **AndroidX MVVM imports** in any file: `androidx.lifecycle.ViewModel`, `androidx.lifecycle.AndroidViewModel`, `androidx.lifecycle.LiveData`, `androidx.lifecycle.MutableLiveData`, `androidx.lifecycle.MediatorLiveData`, `androidx.lifecycle.Observer`, `androidx.lifecycle.ViewModelProvider`, `androidx.lifecycle.LifecycleOwner`.

2. **Hilt annotations**: `@HiltAndroidApp`, `@AndroidEntryPoint`, `@HiltViewModel`, `@Inject`, `@Singleton`, `@Module`, `@Provides`, `@Binds`, `@InstallIn`, `@ApplicationContext`, `@ActivityContext`, `@ActivityRetainedScoped`, `@ActivityScoped`, `@FragmentScoped`, `@ViewModelScoped`. Files annotated `@Module` are configuration files — do not flag any of their imports under layering rules.

3. **Navigation Component**: `androidx.navigation.NavController`, `androidx.navigation.fragment.NavHostFragment`, `androidx.navigation.Navigation`, `androidx.navigation.NavDirections`, `androidx.navigation.fragment.NavHostFragment.findNavController`, `Navigation.findNavController(...)`, calls to `navController.navigate(...)` / `.popBackStack()` / `.navigateUp()`. These are the correct way to navigate.

4. **ViewBinding**: a field whose type ends in `Binding` (e.g., `FragmentHomeBinding binding;`), `<Binding>.inflate(getLayoutInflater(), ...)`, `<Binding>.bind(view)`, `binding.getRoot()`, accessing views via `binding.<id>` properties.

5. **Retrofit service interface files**: an `interface` declaration with methods annotated `@GET`, `@POST`, `@PUT`, `@DELETE`, `@PATCH`, `@HEAD`, or `@OPTIONS` from `retrofit2.http`. Retrofit annotations and imports belong here. Do not flag this file under any "uses Retrofit" rule.

6. **Hilt modules**: a class annotated `@Module` and `@InstallIn(...)` that declares `@Provides` or `@Binds` methods returning `Retrofit`, `OkHttpClient`, `Gson`, API service interfaces, or `Room` databases / DAOs. URL string literals, hardcoded timeouts, and singleton wiring here are not violations.

7. **Application class** (`extends Application`, typically annotated `@HiltAndroidApp`): the entry point of the app. Hardcoded version strings, configuration constants, and static initializers here are not violations.

8. **AndroidViewModel** subclasses: the parent class already holds an `Application` reference accessible via `getApplication()`. Constructor parameter `(@NonNull Application application)` and calls to `getApplication()` are correct — never flag them.

9. **Repository constructor injecting `@ApplicationContext Context context`**: the standard Hilt pattern for Room / SharedPreferences / asset access. The plain `android.content.Context` import is fine in a Repository. Do not flag it under `REPOSITORY_DEPENDS_ON_VIEW_LAYER`.

10. **LiveData observation with `getViewLifecycleOwner()` in a Fragment**: e.g., `viewModel.data.observe(getViewLifecycleOwner(), value -> ...);`. This is the correct lifecycle owner inside a Fragment.

11. **Standard Android lifecycle method names**: `onCreate`, `onCreateView`, `onViewCreated`, `onStart`, `onResume`, `onPause`, `onStop`, `onDestroyView`, `onDestroy`, `onSaveInstanceState`, `onAttach`, `onDetach`. These will often be longer than other methods; judge them by the rules' explicit line thresholds, not by their length alone.

12. **Resource access and developer logging**: `getString(R.string....)`, `getResources()`, `getColor(...)`, `getDrawable(...)`, `Log.d(TAG, "...")` / `Log.e(TAG, "...", e)` with literal tag strings. These are not violations of any rule.

## Rules

For each rule: a trigger (deterministic detection criterion), one BAD example you flag, and one GOOD example you do not flag.

---

### `VIEWMODEL_IMPORTS_ANDROID_UI` — CRITICAL

**Trigger.** The file is a ViewModel (see "Class identification by name suffix"), AND the file contains an `import` line for any of: `android.app.Activity`, `androidx.fragment.app.Fragment`, `androidx.fragment.app.FragmentActivity`, `androidx.appcompat.app.AppCompatActivity`, `android.view.View`, `android.view.ViewGroup`, anything under `android.widget.` (`Button`, `TextView`, `EditText`, `ImageView`, `RecyclerView`, …), or a type ending in `Binding`. A ViewModel must not see the View layer; importing it virtually guarantees a memory leak.

**BAD (flag this):**
```java
import androidx.fragment.app.Fragment;             // ViewModel must not import Fragment
import android.widget.TextView;                    // nor any View / widget
public class HomeViewModel extends ViewModel { ... }
```

**GOOD (do not flag):**
```java
import androidx.lifecycle.ViewModel;
import androidx.lifecycle.MutableLiveData;
import javax.inject.Inject;
@HiltViewModel
public class HomeViewModel extends ViewModel { ... }
```

---

### `VIEWMODEL_HOLDS_CONTEXT` — CRITICAL

**Trigger.** The file declares a class that `extends ViewModel` (NOT `AndroidViewModel` — that is exempt) or is annotated `@HiltViewModel`, AND the class body declares a field whose type is `Context`, `Activity`, `AppCompatActivity`, `FragmentActivity`, `Fragment`, `View`, `ViewGroup`, or any subtype of `View` (e.g., `TextView`, `Button`). The ViewModel outlives the Activity / Fragment, so holding any of these leaks them.

**BAD:**
```java
@HiltViewModel
public class HomeViewModel extends ViewModel {
    private final Context context;                 // leaks the Activity
    @Inject HomeViewModel(Context context) { this.context = context; }
}
```

**GOOD:**
```java
public class HomeViewModel extends AndroidViewModel {
    public HomeViewModel(@NonNull Application application) {
        super(application);                        // Application is process-scoped, no leak
    }
}
```

---

### `FRAGMENT_BYPASSES_VIEWMODEL` — CRITICAL

**Trigger.** The file is a Fragment or an Activity, AND at least one of the following holds:
- The file imports anything from `retrofit2.` (e.g., `retrofit2.Retrofit`, `retrofit2.Call`, `retrofit2.Response`, `retrofit2.Callback`).
- The class declares a field whose type name ends in `Repository`.
- The class declares a field whose type name ends in `ApiService` or `Service`, where the type is referenced from the project (not from `android.app.Service`).

The View layer must talk only to its ViewModel.

**BAD:**
```java
@AndroidEntryPoint
public class HomeFragment extends Fragment {
    @Inject HomeRepository repository;             // Fragment must not hold a Repository
}
```

**GOOD:**
```java
@AndroidEntryPoint
public class HomeFragment extends Fragment {
    private HomeViewModel viewModel;
    @Override public void onViewCreated(@NonNull View v, @Nullable Bundle s) {
        viewModel = new ViewModelProvider(this).get(HomeViewModel.class);
        viewModel.getUsers().observe(getViewLifecycleOwner(), this::renderUsers);
    }
}
```

---

### `REPOSITORY_DEPENDS_ON_VIEW_LAYER` — CRITICAL

**Trigger.** The file is a Repository, AND it imports any of: `android.app.Activity`, `androidx.fragment.app.Fragment`, `androidx.appcompat.app.AppCompatActivity`, `android.view.View`, `android.view.ViewGroup`, anything under `android.widget.`, any class ending in `Activity`, `Fragment`, `ViewModel`, or `Binding`. Plain `android.content.Context` is NOT flagged (see allowlist item 9 — Repositories may take `@ApplicationContext Context`).

**BAD:**
```java
import androidx.fragment.app.Fragment;
import com.example.app.ui.home.HomeViewModel;
public class UserRepository {
    void notify(Fragment f, HomeViewModel vm) { ... }   // Repository must not see UI types
}
```

**GOOD:**
```java
import android.content.Context;                       // OK — used for SharedPreferences / Room
import dagger.hilt.android.qualifiers.ApplicationContext;
@Singleton
public class UserRepository {
    @Inject UserRepository(@ApplicationContext Context context, UserApiService api) { ... }
}
```

---

### `ASYNCTASK_USAGE` — CRITICAL

**Trigger.** The file contains `import android.os.AsyncTask`, OR a class declaration containing `extends AsyncTask`. `AsyncTask` was deprecated in API 30 and removed from new SDKs; the project must use `Executors` + `LiveData`.

**BAD:**
```java
import android.os.AsyncTask;
class LoadUsers extends AsyncTask<Void, Void, List<User>> { ... }
```

**GOOD:**
```java
import java.util.concurrent.Executors;
private final Executor io = Executors.newSingleThreadExecutor();
io.execute(() -> { List<User> users = api.fetch().execute().body(); liveData.postValue(users); });
```

---

### `STATIC_VIEW_OR_CONTEXT_FIELD` — CRITICAL

**Trigger.** The file declares a field with the `static` modifier whose type is `Context`, `Activity`, `AppCompatActivity`, `FragmentActivity`, `Fragment`, `View`, `ViewGroup`, or any subtype of `View`. A `static final String CONSTANT = "..."`, `static final int X = 1`, and similar primitive / `String` constants are NOT flagged. The Application subclass keeping a static reference to itself (`private static MyApp instance;`) is also allowed by the allowlist.

**BAD:**
```java
public class Holder {
    private static Activity sActivity;             // process-wide leak of the Activity
}
```

**GOOD:**
```java
public class Constants {
    public static final String BASE_URL = "https://api.example.com";
    public static final int TIMEOUT_SECONDS = 30;
}
```

---

### `FRAGMENT_BINDING_NOT_NULLED` — CRITICAL

**Trigger.** The file is a Fragment, AND it declares a field whose type name ends in `Binding`, AND the class body does **either** of: (a) does not contain a method named `onDestroyView`; (b) contains `onDestroyView` whose body does not assign the binding field to `null`. The Fragment's View outlives the binding field unless it is released in `onDestroyView`.

**BAD:**
```java
public class HomeFragment extends Fragment {
    private FragmentHomeBinding binding;
    // ... onCreateView assigns binding ...
    // no onDestroyView at all → memory leak
}
```

**GOOD:**
```java
public class HomeFragment extends Fragment {
    private FragmentHomeBinding binding;
    @Override public void onDestroyView() {
        super.onDestroyView();
        binding = null;                            // releases the view tree
    }
}
```

---

### `LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS` — CRITICAL

**Trigger.** The file is a Fragment, AND the file body contains the literal substring `.observe(this,` (or `.observe(this ,` with whitespace). In a Fragment, the lifecycle owner of a LiveData observer MUST be `getViewLifecycleOwner()`; using `this` outlives the View and causes duplicate observers and `IllegalStateException`s when the view is destroyed.

**BAD:**
```java
viewModel.getUsers().observe(this, users -> render(users));   // wrong lifecycle owner
```

**GOOD:**
```java
viewModel.getUsers().observe(getViewLifecycleOwner(), users -> render(users));
```

---

### `VIEWMODEL_BYPASSES_REPOSITORY` — WARNING

**Trigger.** The file is a ViewModel, AND **either** the file imports anything from `retrofit2.` **or** the class declares a field whose type name ends in `ApiService` or `Service` (referenced from the project, not `android.app.Service`). The ViewModel must depend on a Repository, not on Retrofit or an API service directly.

**BAD:**
```java
@HiltViewModel
public class HomeViewModel extends ViewModel {
    @Inject UserApiService api;                    // ViewModel reaches past the Repository
}
```

**GOOD:**
```java
@HiltViewModel
public class HomeViewModel extends ViewModel {
    private final UserRepository repository;
    @Inject HomeViewModel(UserRepository repository) { this.repository = repository; }
}
```

---

### `FINDVIEWBYID_USAGE` — WARNING

**Trigger.** The file is a Fragment or an Activity, AND the file body contains `.findViewById(`. ViewBinding replaces all `findViewById` calls in this project.

**BAD:**
```java
TextView title = view.findViewById(R.id.title);
title.setText("Hello");
```

**GOOD:**
```java
binding.title.setText("Hello");
```

---

### `NAVIGATION_VIA_FRAGMENT_TRANSACTION` — WARNING

**Trigger.** The file is a Fragment or an Activity, AND the file body contains `getSupportFragmentManager().beginTransaction()`, `getChildFragmentManager().beginTransaction()`, `getFragmentManager().beginTransaction()`, OR `requireActivity().getSupportFragmentManager().beginTransaction()` — followed (anywhere on the same chained expression) by `.add(`, `.replace(`, or `.remove(`. The project uses Navigation Component; navigate via `NavController.navigate(...)`. `DialogFragment.show(fragmentManager, "tag")` does NOT use `beginTransaction()` directly and is not flagged.

**BAD:**
```java
getSupportFragmentManager().beginTransaction()
    .replace(R.id.host, new DetailFragment())
    .addToBackStack(null)
    .commit();
```

**GOOD:**
```java
NavHostFragment.findNavController(this)
    .navigate(HomeFragmentDirections.actionHomeToDetail());
```

---

### `FRAGMENT_CASTS_PARENT_ACTIVITY` — WARNING

**Trigger.** The file is a Fragment, AND the file body contains a cast of the form `((<Identifier>) getActivity())` or `((<Identifier>) requireActivity())`, where `<Identifier>` is a PascalCase class name. Fragment-to-Activity coupling should go through a shared ViewModel (`new ViewModelProvider(requireActivity()).get(SharedViewModel.class)`), not a downcast.

**BAD:**
```java
((MainActivity) requireActivity()).showProgressBar();
```

**GOOD:**
```java
SharedViewModel shared = new ViewModelProvider(requireActivity()).get(SharedViewModel.class);
shared.setProgressVisible(true);
```

---

### `RAW_THREAD_FOR_BACKGROUND_WORK` — WARNING

**Trigger.** The file is a Fragment, Activity, ViewModel, or Repository, AND the file body contains `new Thread(` (constructing a raw thread). Per the project's async strategy, background work must use `Executors` (e.g., `Executors.newSingleThreadExecutor()`, `Executors.newFixedThreadPool(...)`) so threads are pooled, named, and shut down properly.

**BAD:**
```java
new Thread(() -> { repository.fetch(); }).start();
```

**GOOD:**
```java
private final Executor io = Executors.newSingleThreadExecutor();
io.execute(() -> { repository.fetch(); });
```

---

### `MUTABLELIVEDATA_EXPOSED_PUBLICLY` — WARNING

**Trigger.** The file is a ViewModel, AND **either** of the following holds:
- The class declares a field with `public` visibility whose type is `MutableLiveData<...>`.
- The class declares a method with `public` visibility whose return type is `MutableLiveData<...>`.

Consumers (Fragments) must only see `LiveData<T>` so they cannot call `setValue` / `postValue`.

**BAD:**
```java
public class HomeViewModel extends ViewModel {
    public MutableLiveData<List<User>> users = new MutableLiveData<>();   // any Fragment can mutate
}
```

**GOOD:**
```java
public class HomeViewModel extends ViewModel {
    private final MutableLiveData<List<User>> users = new MutableLiveData<>();
    public LiveData<List<User>> getUsers() { return users; }
}
```

---

### `FAT_FRAGMENT_OR_ACTIVITY_METHOD` — WARNING

**Trigger.** The file is a Fragment or an Activity, AND some method body contains **more than 40 top-level statements**. Statements are counted at the method's `block` level (direct children classified as `*_statement`, `local_variable_declaration`, or `expression_statement`); nested statements inside `if` / `for` / lambda bodies don't add to the count. Methods at or below 40 top-level statements MUST NOT be flagged regardless of perceived complexity. The threshold is higher than for a Spring controller because Android lifecycle methods legitimately include view binding, observer registration, and click listeners.

**BAD:** an `onViewCreated` method whose body lists 45+ top-level statements (view bindings + LiveData observers + click listeners + navigation setup, all unfactored).

**GOOD:** any Fragment / Activity method whose body has 40 or fewer top-level statements — even if it spans many physical lines through formatting. The threshold is a structural count, not a line count.

---

### `MISSING_HILT_VIEWMODEL` — WARNING

**Trigger.** The file declares a class that `extends ViewModel` or `extends AndroidViewModel`, AND the class has a constructor annotated `@Inject`, AND the class is NOT annotated `@HiltViewModel`. Without `@HiltViewModel`, `ViewModelProvider.get(...)` will not resolve the constructor through Hilt and the ViewModel cannot be obtained.

**BAD:**
```java
public class HomeViewModel extends ViewModel {                 // missing @HiltViewModel
    @Inject HomeViewModel(UserRepository repository) { ... }
}
```

**GOOD:**
```java
@HiltViewModel
public class HomeViewModel extends ViewModel {
    @Inject HomeViewModel(UserRepository repository) { ... }
}
```

---

## Self-check before emitting JSON

For each candidate violation, in one read-through, drop it if any of these is true:

1. Its `rule_id` is not in the RULE_IDS table above.
2. The construct it points at is listed in **Always allowed**.
3. The `severity` does not match the `rule_id`'s fixed severity in the table.
4. `start_line` or `end_line` is not a line that exists in the input file.
5. The same `(rule_id, start_line)` pair already appears in your output list.

Then emit the JSON. Emit nothing else — no preamble, no analysis prose, no markdown fences around the JSON, no trailing comments.
