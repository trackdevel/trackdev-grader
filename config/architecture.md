---
version: 1
---

# Spring Boot rubric

The Spring Boot REST API is graded against a layered/onion architecture
plus a small set of Spring-specific anti-patterns. The structural and
AST checks (`config/architecture.toml`) cover the easy violations; this
section covers the semantic ones an LLM judge is asked to evaluate
per-file.

## Layering

- **Controllers (`@RestController`, `@Controller`) must remain thin.**
  A controller method delegates to a `@Service`; it does not contain
  branching business logic, transactional bookkeeping, or repository
  calls. As a rough guide, a controller method is at most ~20
  statements; longer methods almost always indicate misplaced logic.
- **Controllers must not reach the repository layer directly.** Even
  through a parameter, a field, or a chained call — repository access
  always goes through a service.
- **Services (`@Service`) must not return JPA entities to the
  presentation layer.** They return DTOs (or use a mapper). An entity
  exposed via `@RestController` causes lazy-loading bugs and serialises
  internal state to clients.
- **The persistence layer (`@Repository`, `JpaRepository<>`) must not
  call services or controllers.** Inversion-of-control runs the other
  way; circular references between layers indicate a missing
  abstraction.
- **Domain / model classes must not depend on Spring web, Spring data,
  or `javax.servlet.*`.** The domain layer is portable; framework
  dependencies belong in the layer they enable, not in the model.

## Anti-patterns

- **No `@Autowired` field injection.** Use constructor injection (with
  `final` fields) so the class is testable without Spring and so
  required dependencies are explicit. Setter injection is acceptable
  only for genuine optional collaborators.
- **No business logic inside `@Entity` classes.** Methods on entities
  should be limited to invariant guards (e.g. validation in setters)
  and trivial derived getters. Compute-heavy logic, transactional
  flows, or external IO live in services.
- **Exception handling.** Don't catch and swallow (`} catch (Exception
  e) {}`). Either propagate or convert to a typed domain exception
  with a clear message.
- **Direct DTO ↔ entity conversion in controllers.** Use a dedicated
  mapper (MapStruct, hand-written, doesn't matter) — controllers don't
  reach into entity internals.
- **Hard-coded secrets / API URLs.** Configuration values come from
  `application.yml`, `application.properties`, or environment;
  never inline strings. (The grader is permissive about test fixtures
  inside `src/test/`.)
- **Method parameter validation.** Public REST endpoints validate
  request bodies (`@Valid`) and path/query parameters; missing
  validation is a hidden trust-the-client bug.

# Android rubric

The Android client is graded against MVVM with a Repository pattern. As
above, the AST checks catch the obvious cases (`Activity` holding a
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
  callbacks delegate to `ViewModel` methods; conditional flows live in
  the ViewModel. The Activity / Fragment is a binding layer.
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
  encryption.
- **Activity-scoped state held in static / singleton fields.** Static
  state outlives Activity teardown and leaks contexts. Use the
  framework's lifecycle scoping (`viewModelScope`, `lifecycleScope`).
- **Hard-coded API URLs in client code.** Build-time configuration
  (`buildConfigField`, BuildConfig fields) is the right knob; literal
  URLs in a Retrofit interface are tolerable for a single-environment
  course but should be flagged.

# Severity guidance

The LLM judge (T-P3.3) classifies each violation it finds as `INFO`,
`WARNING`, or `CRITICAL`. As a calibration:

- `CRITICAL` — security-impacting (plaintext tokens, missing
  authorisation), or a layering violation that defeats the entire
  architecture (Activity making Retrofit calls directly into the UI
  thread).
- `WARNING` — clear architectural mistake without a security or
  data-loss consequence (controller calling repository, fat method,
  field injection in production code).
- `INFO` — a code-smell or stylistic issue noteworthy for review but
  not a failed rubric item (long but mostly-trivial method, hard-coded
  URL in a single-environment app).
