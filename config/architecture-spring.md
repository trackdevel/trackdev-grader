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

# Severity guidance

The LLM judge classifies each violation it finds as `INFO`,
`WARNING`, or `CRITICAL`. As a calibration:

- `CRITICAL` — security-impacting (plaintext tokens, missing
  authorisation), or a layering violation that defeats the entire
  architecture (controller calling repository directly with no service
  in between).
- `WARNING` — clear architectural mistake without a security or
  data-loss consequence (controller calling repository, fat method,
  field injection in production code).
- `INFO` — a code-smell or stylistic issue noteworthy for review but
  not a failed rubric item (long but mostly-trivial method, hard-coded
  URL in a single-environment app).
