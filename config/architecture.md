---
version: 7
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
- **Services (`@Service`) must not return JPA entities (`@Entity`) to the
  `@RestController` controller layer.**
  They return DTOs (or use a mapper) to the Controller.
  An entity exposed to a `@RestController` causes lazy-loading bugs and serialises 
  internal state to clients. But Services can return entities if the target
  is another Service
- **The persistence layer (`@Repository`, `JpaRepository<>`) must not
  call services or controllers.** Inversion-of-control runs the other
  way; circular references between layers indicate a missing
  abstraction.
- **Domain / model classes must not depend on Spring web, Spring data
  repositories, or `javax.servlet.*`.** Concretely, do not import
  `org.springframework.web.*`, `org.springframework.data.repository.*`,
  `org.springframework.stereotype.Service`/`@Controller`, or
  `javax.servlet.*` from a class under `model/` / `domain/`.

  **DO NOT REPORT JPA / persistence annotations as a violation.**
  In this course the domain class IS the persistence model.
  The following are *expected* on domain classes and MUST NOT be
  flagged under any rule name (no "domain depends on JPA",
  "domain persistence framework dependency", "domain framework
  coupling", "framework leak in domain", or any rephrased variant):

    - any annotation from `jakarta.persistence.*` or
      `javax.persistence.*` — `@Entity`, `@Table`, `@Id`,
      `@GeneratedValue`, `@Column`, `@JoinColumn`, `@OneToMany`,
      `@ManyToOne`, `@ManyToMany`, `@OneToOne`, `@Embeddable`,
      `@Embedded`, `@Enumerated`, `@Temporal`, `@Transient`, `@Lob`,
      `@MappedSuperclass`, `@Inheritance`, `@DiscriminatorColumn`,
      `@NamedQuery`, `@PrePersist`, `@PostLoad`, etc.
    - any annotation from `jakarta.validation.*` /
      `javax.validation.*` (`@NotNull`, `@Size`, `@Email`, …) and
      `org.hibernate.validator.constraints.*`.
    - any annotation from `org.hibernate.annotations.*` used purely
      for ORM mapping (e.g. `@Type`, `@CreationTimestamp`).

  These imports are persistence *mapping*, not a framework leak.
  The line is: persistence mapping on the entity is fine; reaching
  for the Spring web/MVC stack or the data *repository* abstraction
  from the entity is not.

## Anti-patterns

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
- **Hard-coded *secrets* (passwords, tokens, signing keys, OAuth
  client secrets).** Secrets must come from `application.yml`,
  `application.properties`, or the environment. **Hard-coded API
  URLs and non-secret configuration values are allowed in this
  course** — this is a single-environment student project, so a
  literal API base URL, a hard-coded request-logging payload limit,
  a hard-coded port, or other tuning constants are NOT a violation.
  DO NOT REPORT URLs, endpoint paths, log-format strings, payload
  limits, boolean toggles, or any other non-secret configuration as
  hard-coded violations under any rule name (no "HARDCODED API URL",
  "HARDCODED API ENDPOINTS", "HARDCODED CONFIG", or rephrased
  variant).
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
