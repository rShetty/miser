import type { ComplexityTier } from "@miser/config";

export interface BenchmarkPrompt {
  id: string;
  tier: ComplexityTier;
  prompt: string;
  messages: Array<{ role: string; content: string }>;
}

export const BENCHMARK_PROMPTS: BenchmarkPrompt[] = [
  {
    id: "trivial-1",
    tier: "trivial",
    prompt: "Hello",
    messages: [{ role: "user", content: "Hello" }],
  },
  {
    id: "trivial-2",
    tier: "trivial",
    prompt: "What is 2+2?",
    messages: [{ role: "user", content: "What is 2+2?" }],
  },
  {
    id: "trivial-3",
    tier: "trivial",
    prompt: "git status",
    messages: [{ role: "user", content: "git status" }],
  },
  {
    id: "trivial-4",
    tier: "trivial",
    prompt: "Thanks!",
    messages: [{ role: "user", content: "Thanks!" }],
  },
  {
    id: "simple-1",
    tier: "simple",
    prompt: "Explain what DNS is",
    messages: [{ role: "user", content: "Explain what DNS is" }],
  },
  {
    id: "simple-2",
    tier: "simple",
    prompt: "Write a Python function to reverse a string",
    messages: [{ role: "user", content: "Write a Python function to reverse a string" }],
  },
  {
    id: "simple-3",
    tier: "simple",
    prompt: "Compare React vs Vue for a small project",
    messages: [{ role: "user", content: "Compare React vs Vue for a small project" }],
  },
  {
    id: "simple-4",
    tier: "simple",
    prompt: "Add a comment to this function explaining what it does",
    messages: [{ role: "user", content: "Add a comment to this function explaining what it does" }],
  },
  {
    id: "standard-1",
    tier: "standard",
    prompt: "Implement a rate limiting middleware for Express.js",
    messages: [{ role: "user", content: "Implement a rate limiting middleware for Express.js" }],
  },
  {
    id: "standard-2",
    tier: "standard",
    prompt: "Debug why my Node.js app crashes with ECONNRESET on startup",
    messages: [{ role: "user", content: "Debug why my Node.js app crashes with ECONNRESET on startup" }],
  },
  {
    id: "standard-3",
    tier: "standard",
    prompt: "Write integration tests for a REST API with authentication",
    messages: [{ role: "user", content: "Write integration tests for a REST API with authentication" }],
  },
  {
    id: "standard-4",
    tier: "standard",
    prompt: "Refactor this module to use dependency injection",
    messages: [{ role: "user", content: "Refactor this module to use dependency injection" }],
  },
  {
    id: "hard-1",
    tier: "hard",
    prompt: "Architect a distributed caching system that handles cache invalidation across 5 regions",
    messages: [{ role: "user", content: "Architect a distributed caching system that handles cache invalidation across 5 regions" }],
  },
  {
    id: "hard-2",
    tier: "hard",
    prompt: "Analyze the root cause of a production incident where the database connection pool was exhausted",
    messages: [{ role: "user", content: "Analyze the root cause of a production incident where the database connection pool was exhausted" }],
  },
  {
    id: "hard-3",
    tier: "hard",
    prompt: "Review the security of this authentication flow and identify potential vulnerabilities",
    messages: [{ role: "user", content: "Review the security of this authentication flow and identify potential vulnerabilities" }],
  },
  {
    id: "reasoning-1",
    tier: "reasoning",
    prompt: "Prove that the square root of 2 is irrational",
    messages: [{ role: "user", content: "Prove that the square root of 2 is irrational" }],
  },
  {
    id: "reasoning-2",
    tier: "reasoning",
    prompt: "Design a consensus algorithm that handles Byzantine faults for a network of 7 nodes",
    messages: [{ role: "user", content: "Design a consensus algorithm that handles Byzantine faults for a network of 7 nodes" }],
  },
  {
    id: "reasoning-3",
    tier: "reasoning",
    prompt: "Analyze the time complexity of this algorithm and prove it is O(n log n)",
    messages: [{ role: "user", content: "Analyze the time complexity of this algorithm and prove it is O(n log n)" }],
  },
];
