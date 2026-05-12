---
rubric_version: 8
target_model: claude-haiku
target_stack: spring-boot-3.x / java-21
scope: single-file analysis
---

> **TARGETING.** This file is the human-readable spec for the AST rules
> in `config/architecture.toml`. As of Wave 4 of the AST-rubric
> migration it is **no longer fed to an LLM**. The deterministic AST
> engine in `crates/architecture/src/ast_rules.rs` is authoritative;
> this document is reference material for the instructor, and the
> golden source for the `crates/architecture/tests/spring_v8_fixtures.rs`
> integration tests. Bump `rubric_version` and tune the corresponding
> `[[ast_rule]]` block in `architecture.toml` when the policy changes.

# Spring Boot Architecture Rubric

## Task

You are reviewing ONE Java source file from a Spring Boot 3.x / Java 21 project for architectural violations. You do not see other files. Do not infer cross-file relationships beyond what the imports show.

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
| `CONTROLLER_RETURNS_ENTITY` | CRITICAL |
| `CONTROLLER_USES_REPOSITORY` | CRITICAL |
| `CONTROLLER_HAS_TRANSACTIONAL` | CRITICAL |
| `TRANSACTIONAL_ON_NON_PUBLIC_METHOD` | CRITICAL |
| `UNBOUNDED_FIND_ALL` | CRITICAL |
| `ENTITY_USES_LOMBOK_DATA` | CRITICAL |
| `ENTITY_USES_JAVAX_IMPORT` | CRITICAL |
| `FAT_CONTROLLER_METHOD` | WARNING |
| `MANUAL_DTO_MAPPING_IN_CONTROLLER` | WARNING |
| `MISSING_VALID_ON_REQUEST_BODY` | WARNING |
| `SERVICE_PUBLIC_METHOD_USES_ENTITY` | WARNING |
| `SERVICE_USES_MULTIPLE_REPOSITORIES` | WARNING |
| `ENTITY_DEPENDS_ON_SPRING_BEAN` | WARNING |

If a candidate finding does not fit one of these `rule_id` values, **do not emit it**. Do not invent new `rule_id` values. Do not rephrase a `rule_id` under a different name.

## Always allowed (never emit a violation for any of these)

These constructs are part of the expected architecture. Emit no violation that references any of them under any `rule_id`. If your candidate finding matches one of these, drop it before output.

1. **JPA annotations on `@Entity` classes** (`jakarta.persistence.*`):
   `@Entity`, `@Table`, `@Id`, `@GeneratedValue`, `@Column`, `@JoinColumn`,
   `@OneToMany`, `@ManyToOne`, `@ManyToMany`, `@OneToOne`, `@JoinTable`,
   `@Embeddable`, `@Embedded`, `@EmbeddedId`, `@MapsId`,
   `@Enumerated`, `@Temporal`, `@Transient`, `@Lob`,
   `@MappedSuperclass`, `@Inheritance`, `@DiscriminatorColumn`, `@DiscriminatorValue`,
   `@NamedQuery`, `@NamedQueries`,
   `@PrePersist`, `@PostLoad`, `@PreUpdate`, `@PostPersist`, `@PreRemove`,
   `@Version`.

2. **Hibernate annotations on entities** (`org.hibernate.annotations.*`):
   `@CreationTimestamp`, `@UpdateTimestamp`, `@Type`, `@JdbcTypeCode`, `@JdbcType`, `@Formula`, `@NaturalId`.

3. **Validation annotations** (`jakarta.validation.constraints.*` and `jakarta.validation.Valid`):
   `@NotNull`, `@NotBlank`, `@NotEmpty`, `@Size`, `@Email`, `@Min`, `@Max`,
   `@Positive`, `@Negative`, `@Pattern`, `@Past`, `@Future`, `@Digits`, `@Valid`.

4. **Constructor injection**: one constructor that takes dependencies, with or without `@Autowired`, or Lombok `@RequiredArgsConstructor` / `@AllArgsConstructor` on the class. This is the correct pattern. Never flag it as `FIELD_INJECTION`.

5. **`@Transactional` on a `public` service method**: any propagation, with or without `readOnly = true`, with or without `rollbackFor`. It belongs there.

6. **URL strings and path templates** in `@GetMapping`, `@PostMapping`, `@PutMapping`, `@DeleteMapping`, `@PatchMapping`, `@RequestMapping`, `@PathVariable`, `@RequestParam`. Path literals like `"/users/{id}"`, full URLs like `"https://api.example.com"`, integer literals (ports, sizes, timeouts), and boolean literals are not violations.

7. **`@RestControllerAdvice` / `@ControllerAdvice` classes** that catch exceptions via `try/catch` or `@ExceptionHandler`. Centralised exception handling is correct here.

8. **MapStruct `@Mapper` interfaces** (annotated `@Mapper(componentModel = "spring")` or similar). These legitimately convert between `@Entity` and DTO; do not flag them under `MANUAL_DTO_MAPPING_IN_CONTROLLER` or `SERVICE_PUBLIC_METHOD_USES_ENTITY`.

9. **DTOs as Java `record` types**, including records with validation annotations on components. Records are the recommended DTO form.

## Rules

For each rule: a trigger (deterministic detection criterion), one BAD example you flag, and one GOOD example you do not flag.

---

### `CONTROLLER_RETURNS_ENTITY` — CRITICAL

**Trigger.** The file contains `@RestController` or `@Controller`. There exists a public method on that class whose return type text contains the name `T`, where `T` is either (a) a class declared in this file with the `@Entity` annotation, or (b) a type imported from a package whose path contains `.entity.`, `.entities.`, `.domain.`, or `.model.`. Generic wrappers (`ResponseEntity<T>`, `Optional<T>`, `List<T>`, `Page<T>`, `Mono<T>`, `Flux<T>`) do not protect the inner type.

**BAD (flag this):**
```java
@RestController
class UserController {
    @GetMapping("/{id}")
    public ResponseEntity<User> get(@PathVariable Long id) {   // User is @Entity
        return ResponseEntity.ok(service.find(id));
    }
}
```

**GOOD (do not flag):**
```java
@RestController
class UserController {
    @GetMapping("/{id}")
    public ResponseEntity<UserResponse> get(@PathVariable Long id) {
        return ResponseEntity.ok(service.find(id));
    }
}
```

---

### `CONTROLLER_USES_REPOSITORY` — CRITICAL

**Trigger.** A class annotated `@RestController` or `@Controller` declares a field, constructor parameter, or setter parameter whose type name ends in `Repository`, or whose type is an interface that the file shows extending `JpaRepository`, `CrudRepository`, `PagingAndSortingRepository`, or `Repository`.

**BAD:**
```java
@RestController
class UserController {
    private final UserRepository userRepository;   // direct repository in controller
}
```

**GOOD:**
```java
@RestController
class UserController {
    private final UserService userService;
}
```

---

### `CONTROLLER_HAS_TRANSACTIONAL` — CRITICAL

**Trigger.** The file contains `@RestController` or `@Controller`, and `@Transactional` appears on that class or on any of its methods. Source package is irrelevant (`org.springframework.transaction.annotation.Transactional` or `jakarta.transaction.Transactional`).

**BAD:**
```java
@RestController
class OrderController {
    @Transactional
    @PostMapping
    public OrderResponse create(@RequestBody OrderRequest req) { ... }
}
```

**GOOD:** `@Transactional` lives on a `@Service` method. The controller file contains no `@Transactional`.

---

### `TRANSACTIONAL_ON_NON_PUBLIC_METHOD` — CRITICAL

**Trigger.** `@Transactional` annotation directly precedes a method declaration whose visibility is `private`, `protected`, or absent (package-private). Spring AOP cannot intercept it; the annotation is silently ignored at runtime.

**BAD:**
```java
@Service
class OrderService {
    @Transactional
    void saveOrder(Order o) { ... }     // package-private — not intercepted
}
```

**GOOD:**
```java
@Service
class OrderService {
    @Transactional
    public void saveOrder(Order o) { ... }
}
```

---

### `UNBOUNDED_FIND_ALL` — CRITICAL

**Trigger.** A call of the form `<identifier>.findAll()` with zero arguments appears inside a class annotated `@RestController`, `@Controller`, `@Service`, or `@Component`. Calls with arguments (`findAll(pageable)`, `findAll(spec, pageable)`, `findAll(example)`) do not trigger.

**BAD:**
```java
public List<UserResponse> list() {
    return userRepository.findAll()                 // unbounded
        .stream().map(mapper::toResponse).toList();
}
```

**GOOD:**
```java
public Page<UserResponse> list(Pageable pageable) {
    return userRepository.findAll(pageable).map(mapper::toResponse);
}
```

---

### `ENTITY_USES_LOMBOK_DATA` — CRITICAL

**Trigger.** A class annotated `@Entity` carries at least one of:
- `@Data` from Lombok;
- `@EqualsAndHashCode` without the parameter `onlyExplicitlyIncluded = true`;
- `@ToString` without an `exclude` parameter listing every `@OneToMany` / `@ManyToMany` collection field in the class.

**BAD:**
```java
@Entity
@Data                                              // pulls in equals/hashCode/toString over all fields
public class Post {
    @Id private Long id;
    @OneToMany(mappedBy = "post") private Set<Comment> comments;
}
```

**GOOD:**
```java
@Entity
@Getter @Setter
@EqualsAndHashCode(of = "id")
@ToString(exclude = "comments")
public class Post {
    @Id private Long id;
    @OneToMany(mappedBy = "post") private Set<Comment> comments;
}
```

---

### `ENTITY_USES_JAVAX_IMPORT` — CRITICAL

**Trigger.** The file contains any line matching `import javax.persistence.` or `import javax.validation.`. Spring Boot 3 requires `jakarta.*`; `javax.*` annotations are ignored.

**BAD:**
```java
import javax.persistence.Entity;
import javax.validation.constraints.NotNull;
```

**GOOD:**
```java
import jakarta.persistence.Entity;
import jakarta.validation.constraints.NotNull;
```

---

### `FAT_CONTROLLER_METHOD` — WARNING

**Trigger.** Inside a `@RestController` or `@Controller` class, a method annotated with `@GetMapping`, `@PostMapping`, `@PutMapping`, `@DeleteMapping`, `@PatchMapping`, or `@RequestMapping` has a method body whose closing `}` is **more than 25 lines** below its opening `{`. Methods whose body span is ≤ 25 lines MUST NOT be flagged regardless of perceived complexity.

**BAD:** a `@PostMapping` method whose body opens at line 30 and closes at line 75 (45 lines).

**GOOD:** any controller method whose body spans 25 lines or fewer, even if it looks dense.

---

### `MANUAL_DTO_MAPPING_IN_CONTROLLER` — WARNING

**Trigger.** Inside a `@RestController` / `@Controller` class body, one of:
- A `new <Name>Dto(...)`, `new <Name>Response(...)`, or `new <Name>Request(...)` constructor call passing two or more arguments where at least one argument is a method call of the form `<id>.get<X>()`.
- A `.map(<id> -> new <Name>(Dto|Response|Request)(...))` lambda.

**BAD:**
```java
return new UserResponse(user.getId(), user.getEmail(), user.getName());
```

**GOOD:**
```java
return userMapper.toResponse(user);    // userMapper is a MapStruct @Mapper
```

---

### `MISSING_VALID_ON_REQUEST_BODY` — WARNING

**Trigger.** A method parameter declaration contains `@RequestBody` whose same-parameter annotation list does NOT also contain `@Valid` or `@Validated`.

**BAD:**
```java
@PostMapping
public X create(@RequestBody CreateXRequest req) { ... }
```

**GOOD:**
```java
@PostMapping
public X create(@Valid @RequestBody CreateXRequest req) { ... }
```

---

### `SERVICE_PUBLIC_METHOD_USES_ENTITY` — WARNING

**Trigger.** A class annotated `@Service` has a method declared `public` whose return type or any parameter type names `T`, where `T` is (a) a class declared in this file with `@Entity`, or (b) a type imported from a package whose path contains `.entity.`, `.entities.`, `.domain.`, or `.model.`. Generic wrappers do not protect the inner type. Methods declared `private`, `protected`, or package-private (no modifier) are NOT flagged — they may legitimately exchange entities with collaborators.

**BAD:**
```java
@Service
class UserService {
    public User create(User u) { ... }       // public + entity in signature
}
```

**GOOD:**
```java
@Service
class UserService {
    public UserResponse create(CreateUserRequest r) { ... }
    User loadInternal(Long id) { ... }       // package-private — allowed
}
```

---

### `SERVICE_USES_MULTIPLE_REPOSITORIES` — WARNING

**Trigger.** A class annotated `@Service` declares more than one field whose type name ends in `Repository`, or whose type is imported from a `.repository` package. (Architectural rule: one repository per service; cross-aggregate access must go through another `@Service`.)

**BAD:**
```java
@Service
@RequiredArgsConstructor
class OrderService {
    private final OrderRepository orderRepository;
    private final UserRepository userRepository;   // second repository → flag
}
```

**GOOD:**
```java
@Service
@RequiredArgsConstructor
class OrderService {
    private final OrderRepository orderRepository;
    private final UserService userService;         // collaborate via service
}
```

---

### `ENTITY_DEPENDS_ON_SPRING_BEAN` — WARNING

**Trigger.** A class annotated `@Entity` contains at least one of:
- An `@Autowired` annotation anywhere in the class body;
- A field whose type name ends in `Service`, `Repository`, or `Component`;
- An `import org.springframework.stereotype.Service`, `import org.springframework.stereotype.Component`, or similar Spring stereotype import.

**BAD:**
```java
@Entity
class Order {
    @Autowired @Transient private PricingService pricingService;
    public BigDecimal total() { return pricingService.compute(this); }
}
```

**GOOD:** entities are persistent POJOs. Move logic that needs services into a `@Service`.

---

## Self-check before emitting JSON

For each candidate violation, in one read-through, drop it if any of these is true:

1. Its `rule_id` is not in the RULE_IDS table above.
2. The construct it points at is listed in **Always allowed**.
3. The `severity` does not match the `rule_id`'s fixed severity in the table.
4. `start_line` or `end_line` is not a line that exists in the input file.
5. The same `(rule_id, start_line)` pair already appears in your output list.

Then emit the JSON. Emit nothing else — no preamble, no analysis prose, no markdown fences around the JSON, no trailing comments.
