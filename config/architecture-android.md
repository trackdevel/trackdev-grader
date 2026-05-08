---
version: 7
---

# Android rubric

The Android client is graded against MVVM with a Repository pattern. As
elsewhere, the AST checks catch the obvious cases (`Activity` holding a
`Retrofit`, `ViewModel` holding an `ApiService`); this section covers
the semantic and design-level checks.

## Layering

- **Activities and Fragments access data only through a Repository.**
  Direct calls to `Retrofit`, `Room` DAOs, or shared preferences from
  UI classes break testability and re-usability across screens.
- **ViewModels do not depend on Activity / Fragment / Context.**
  `ViewModel`s outlive configuration changes; holding a reference to a
  destroyed Activity is a memory leak. Use `AndroidViewModel` only
  when the application Context is genuinely needed (resources,
  application-scoped services).
- **UI does not contain business logic.** Click handlers and lifecycle
  callbacks delegate to `ViewModel` methods; *business* conditional
  flows (validation, computation, persistence decisions) live in the
  ViewModel. **Presentation logic stays in the UI**: observing
  `LiveData` / `StateFlow` and branching on the emitted state to update
  views (e.g. `when (result) { Success -> showData(...); Loading ->
  showSpinner(); Error -> showMessage(...) }`) is the correct MVVM
  pattern, not a violation. Likewise, view-binding conditionals
  (visibility toggles, formatting, navigation dispatch from a click)
  are UI concerns. The line is: *the UI binds and reacts; the
  ViewModel decides*.
- **Repositories own caching and conflict resolution.** A Repository
  decides whether to read from network, Room, or memory; ViewModels
  ask the Repository for data and don't see the cache decision.

## Anti-patterns

- **`runOnUiThread` / `Handler(Looper.getMainLooper())` outside
  Repository / ViewModel scaffolding.** UI threading is the framework's
  job (LiveData, StateFlow, coroutines on `Main`); reaching for it
  manually is a sign that the surrounding architecture is wrong.
- **Network or DB calls on the main thread.** The framework will
  throw, but it's worth flagging early — `Dispatchers.IO`, RxJava
  schedulers, or `Executor.execute` belong in the data layer.
- **`AsyncTask` or `Thread.start()` in new code.** Deprecated for
  years; coroutines or RxJava are the modern replacements.
- **Persisting auth tokens in `SharedPreferences` without encryption.**
  Use `EncryptedSharedPreferences` or `DataStore` with explicit
  encryption. **Persisting *cookies* (e.g. a `PersistentCookieJar`
  that serialises `okhttp3.Cookie` objects to `SharedPreferences` or
  the filesystem) is NOT a violation in this course** — session
  cookies for the team's own backend are out of scope. DO NOT REPORT
  cookie persistence under any rule name (no "UNENCRYPTED COOKIE
  SAVE", "COOKIE_SAVED_PLAINTEXT", "INSECURE_COOKIE_PERSISTENCE", or
  rephrased variant). Only flag plaintext persistence of **bearer
  tokens / API keys / refresh tokens / passwords** stored as their
  own preference values.
- **Activity-scoped state held in static / singleton fields.** Static
  state outlives Activity teardown and leaks contexts. Use the
  framework's lifecycle scoping (`viewModelScope`, `lifecycleScope`).
- **Hard-coded *secrets* in client code (API keys, OAuth client
  secrets, signing keys).** Use `BuildConfig` fields populated from
  `local.properties` / a keystore, or fetch at runtime. **Hard-coded
  API URLs, endpoint paths, and non-secret configuration values are
  allowed in this course** — this is a single-environment student
  project. DO NOT REPORT a literal base URL in a Retrofit interface,
  endpoint path strings on `@GET` / `@POST` annotations, an adapter
  building an image URL from a literal host, or other non-secret
  configuration as a violation under any rule name (no "HARDCODED
  API URL", "HARDCODED API ENDPOINTS", or rephrased variant).

# Severity guidance

The LLM judge classifies each violation it finds as `INFO`,
`WARNING`, or `CRITICAL`. As a calibration:

- `CRITICAL` — security-impacting (plaintext tokens, missing
  authorisation), or a layering violation that defeats the entire
  architecture (Activity making Retrofit calls directly into the UI
  thread).
- `WARNING` — clear architectural mistake without a security or
  data-loss consequence (fragment calling Retrofit through a
  repository-shaped wrapper, manual UI threading in business code).
- `INFO` — a code-smell or stylistic issue noteworthy for review but
  not a failed rubric item (long but mostly-trivial method, hard-coded
  URL in a single-environment app).
