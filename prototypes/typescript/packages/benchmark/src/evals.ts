import type { ClassifiableRequest } from "@miser/classifier";
import type { ComplexityTier } from "@miser/config";

export interface EvalCase {
  id: string;
  category: string;
  expected: ComplexityTier;
  request: ClassifiableRequest;
}

const user = (content: string): ClassifiableRequest => ({ messages: [{ role: "user", content }] });
const item = (id: string, category: string, expected: ComplexityTier, request: ClassifiableRequest): EvalCase => ({ id, category, expected, request });

export const EVAL_CASES: EvalCase[] = [
  item("t01", "basic", "trivial", user("Hello")),
  item("t02", "basic", "trivial", user("Thanks, that worked")),
  item("t03", "basic", "trivial", user("What is 7 times 8?")),
  item("t04", "basic", "trivial", user("What is the capital of Japan?")),
  item("t05", "coding", "trivial", user("Run git status")),
  item("t06", "coding", "trivial", user("List the files in src")),
  item("t07", "coding", "trivial", user("Rename the variable tmp to result in this function")),
  item("t08", "coding", "trivial", user("Change this string from lowercase to uppercase")),
  item("t09", "basic", "trivial", user("Answer yes or no: is 11 prime?")),
  item("t10", "coding", "trivial", user("Show the latest git commit")),
  item("t11", "adversarial", "trivial", user("In one sentence, define Byzantine fault tolerance")),
  item("t12", "adversarial", "trivial", user("Spell the word architecture")),
  item("t13", "adversarial", "trivial", user("Does the phrase 'security vulnerability' contain 21 characters?")),
  item("t14", "basic", "trivial", user("Convert 3 PM UTC to 10 AM EST")),
  item("t15", "coding", "trivial", user("Delete the trailing whitespace on this line")),

  item("s01", "explanation", "simple", user("Explain DNS to a junior developer")),
  item("s02", "coding", "simple", user("Write a TypeScript function that reverses a string")),
  item("s03", "coding", "simple", user("Add a null check before reading user.name")),
  item("s04", "writing", "simple", user("Summarize this paragraph in three bullet points")),
  item("s05", "comparison", "simple", user("Compare REST and GraphQL at a high level")),
  item("s06", "coding", "simple", user("Convert this Python list comprehension to a for loop")),
  item("s07", "coding", "simple", user("Write a regex that matches six digit ZIP codes")),
  item("s08", "coding", "simple", user("Create a TypeScript interface for a user with id, name, and email")),
  item("s09", "writing", "simple", user("Rewrite this email to sound more concise and polite")),
  item("s10", "coding", "simple", user("Explain why this loop runs ten times")),
  item("s11", "adversarial", "simple", user("Explain what a race condition is without diagnosing any code")),
  item("s12", "adversarial", "simple", user("Give a two-sentence overview of distributed caching")),
  item("s13", "coding", "simple", user("Write a shell command to find all .log files")),
  item("s14", "coding", "simple", user("Format this JSON and sort its top-level keys")),
  item("s15", "data", "simple", user("Convert this CSV row into a JSON object")),

  item("n01", "coding", "standard", user("Implement rate limiting middleware for an Express API with unit tests")),
  item("n02", "debugging", "standard", user("Debug why this Node service returns ECONNRESET during startup")),
  item("n03", "testing", "standard", user("Write integration tests for a REST API with JWT authentication")),
  item("n04", "coding", "standard", user("Refactor this module to use dependency injection without changing behavior")),
  item("n05", "database", "standard", user("Add cursor pagination to this PostgreSQL-backed endpoint")),
  item("n06", "coding", "standard", user("Implement file upload validation and error handling in this service")),
  item("n07", "debugging", "standard", user("Find and fix the memory leak in this React component")),
  item("n08", "testing", "standard", user("Create end-to-end tests for checkout using the existing test conventions")),
  item("n09", "database", "standard", user("Design tables and migrations for users, teams, and memberships")),
  item("n10", "coding", "standard", user("Add OAuth login to this existing web application")),
  item("n11", "adversarial", "standard", user("Implement a straightforward mutex-protected in-memory queue for one process")),
  item("n12", "adversarial", "standard", user("Build a small demo of Raft leader election; correctness under partitions is not required")),
  item("n13", "multi-turn", "standard", { messages: [{ role: "user", content: "We added caching yesterday." }, { role: "assistant", content: "What is failing?" }, { role: "user", content: "Update the existing tests and fix stale reads after writes." }] }),
  item("n14", "structured", "standard", { ...user("Extract customer name, invoice number, and total from this text"), response_format: { type: "json_schema" } }),
  item("n15", "tools", "standard", { ...user("Find the failing test, edit the implementation, and rerun the test"), tools: [{ type: "function", function: { name: "read_file", description: "Read a file" } }, { type: "function", function: { name: "run_tests", description: "Run tests" } }] }),

  item("h01", "architecture", "hard", user("Architect a distributed cache with invalidation across five regions and explain consistency tradeoffs")),
  item("h02", "incident", "hard", user("Analyze a production incident where database pools exhausted across 40 services and propose remediation")),
  item("h03", "security", "hard", user("Threat-model this authentication system and identify exploitable trust-boundary failures")),
  item("h04", "migration", "hard", user("Plan a zero-downtime migration from a monolith to services while preserving transactional behavior")),
  item("h05", "concurrency", "hard", user("Diagnose an intermittent race condition spanning workers, Redis locks, and database transactions")),
  item("h06", "architecture", "hard", user("Design a multi-tenant event platform handling one million events per second with regional failover")),
  item("h07", "coding", "hard", user("Refactor this 80-file compiler subsystem while preserving its public API and test behavior")),
  item("h08", "performance", "hard", user("Profile and redesign a latency-critical query path whose p99 regressed from 80ms to 2s")),
  item("h09", "security", "hard", user("Review this cryptographic protocol integration for nonce reuse, downgrade, and replay risks")),
  item("h10", "incident", "hard", user("Develop a root-cause analysis for duplicate payments involving retries, queues, and idempotency races")),
  item("h11", "adversarial", "hard", user("We need a practical production design for Byzantine-tolerant replication; a formal proof is out of scope")),
  item("h12", "architecture", "hard", user("Choose and justify a sharding strategy for a rapidly growing social graph")),
  item("h13", "multi-turn", "hard", { messages: [{ role: "system", content: "You maintain a large production payments repository." }, { role: "user", content: "Retries caused double charges." }, { role: "assistant", content: "Which components are involved?" }, { role: "user", content: "API, Kafka consumers, Redis locks, and two databases. Trace the failure and design a safe fix." }] }),
  item("h14", "tools", "hard", { ...user("Investigate the production outage across the repository, inspect logs, patch the root cause, and validate the rollout"), tools: [{ type: "function", function: { name: "grep", description: "Search repository" } }, { type: "function", function: { name: "shell", description: "Run commands" } }, { type: "function", function: { name: "deploy", description: "Deploy service" } }] }),
  item("h15", "structured", "hard", { messages: [{ role: "system", content: "Return a complete migration plan that satisfies all listed invariants and rollback constraints.".repeat(35) }, { role: "user", content: "Migrate the globally replicated billing database without downtime." }], response_format: { type: "json_schema" } }),

  item("r01", "math", "reasoning", user("Prove that the square root of 2 is irrational")),
  item("r02", "algorithms", "reasoning", user("Design an algorithm for this constrained scheduling problem and prove its optimality")),
  item("r03", "distributed", "reasoning", user("Prove the safety and liveness properties of this Byzantine consensus protocol")),
  item("r04", "logic", "reasoning", user("Determine whether this first-order logic theory is satisfiable and justify each inference")),
  item("r05", "algorithms", "reasoning", user("Derive the tight asymptotic bound for this recurrence using two independent methods")),
  item("r06", "math", "reasoning", user("Find a counterexample to this graph-theory conjecture or prove it holds")),
  item("r07", "optimization", "reasoning", user("Formulate this allocation problem as an integer program and derive a valid relaxation bound")),
  item("r08", "algorithms", "reasoning", user("Invent a subquadratic algorithm for this sequence problem and prove correctness")),
  item("r09", "distributed", "reasoning", user("Show whether exactly-once delivery is achievable under these crash and network assumptions")),
  item("r10", "probability", "reasoning", user("Derive the posterior distribution and prove the estimator is unbiased")),
  item("r11", "adversarial", "reasoning", user("Do not use the word proof, but establish rigorously that this algorithm always terminates and returns the optimum")),
  item("r12", "logic", "reasoning", user("Construct a reduction from 3-SAT to this decision problem and establish both directions")),
  item("r13", "math", "reasoning", user("Solve this functional equation over the reals and show there are no other solutions")),
  item("r14", "optimization", "reasoning", user("Given conflicting constraints, find the minimum-cost feasible assignment and certify optimality")),
  item("r15", "algorithms", "reasoning", user("Analyze this randomized algorithm's expected runtime and high-probability bound")),

  item("o01", "override", "trivial", user("@fast architect a globally distributed database")),
  item("o02", "override", "reasoning", user("@deep what is 2+2?")),
  item("o03", "override", "hard", user("@hard say hello")),
  item("o04", "override", "simple", user("@simple prove Fermat's last theorem")),
  item("o05", "override", "standard", user("@standard spell cat")),
];
