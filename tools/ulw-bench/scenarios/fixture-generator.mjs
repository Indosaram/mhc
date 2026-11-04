// SPDX-License-Identifier: MIT

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const CARGO_TOML = `[package]
name = "ulw-bench-fixture"
version = "0.1.0"
edition = "2021"

[dependencies]
`;

const LIB_RS = `pub mod cli;
pub mod math;
pub mod parser_a;
pub mod parser_b;
pub mod sort;
pub mod unused;

pub fn run() {
    println!("ulw-bench-fixture running");
}
`;

const MATH_RS = `pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        // Intentional failing test for scenario S2
        assert_eq!(add(2, 2), 5);
    }

    #[test]
    fn test_subtract() {
        assert_eq!(subtract(5, 3), 2);
    }
}
`;

const CLI_RS = `pub struct CliConfig {
    pub output: String,
}

impl CliConfig {
    pub fn new() -> Self {
        Self {
            output: String::from("default"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_config() {
        let config = CliConfig::new();
        assert_eq!(config.output, "default");
    }
}
`;

const SORT_RS = `/// Quicksort implementation using the Lomuto partition scheme.
///
/// Time Complexity:
/// - Worst-case: O(n^2) when elements are already sorted or reverse sorted with an edge pivot.
/// - Average-case: O(n log n).
/// - Best-case: O(n log n).
pub fn quicksort<T: Ord>(arr: &mut [T]) {
    if arr.len() <= 1 {
        return;
    }
    let pivot_index = partition(arr);
    let (left, right) = arr.split_at_mut(pivot_index);
    quicksort(left);
    quicksort(&mut right[1..]);
}

fn partition<T: Ord>(arr: &mut [T]) -> usize {
    let len = arr.len();
    let mut i = 0;
    for j in 0..len - 1 {
        if arr[j] <= arr[len - 1] {
            arr.swap(i, j);
            i += 1;
        }
    }
    arr.swap(i, len - 1);
    i
}
`;

const PARSER_A_RS = `/// Parser A: Recursive descent parser for simple arithmetic expressions.
///
/// Characteristics:
/// - Architecture: Top-down recursive descent with operator precedence.
/// - Strengths: High readability, descriptive syntax error positions, intuitive AST mapping.
/// - Trade-offs: Recursive call overhead on deeply nested expressions; stack depth proportional to expression depth.
#[derive(Debug, PartialEq)]
pub enum Expr {
    Number(i64),
    Binary(Box<Expr>, char, Box<Expr>),
}

pub struct RecursiveDescentParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> RecursiveDescentParser<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Expr, String> {
        self.parse_add_sub()
    }

    fn parse_add_sub(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_number()?;
        while self.pos < self.input.len() {
            let op = self.input.as_bytes()[self.pos] as char;
            if op == '+' || op == '-' {
                self.pos += 1;
                let right = self.parse_number()?;
                left = Expr::Binary(Box::new(left), op, Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_number(&mut self) -> Result<Expr, String> {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b' ' {
            self.pos += 1;
        }
        let start = self.pos;
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(format!("Expected digit at position {}", start));
        }
        let num: i64 = self.input[start..self.pos]
            .parse()
            .map_err(|e| format!("{}", e))?;
        Ok(Expr::Number(num))
    }
}
`;

const PARSER_B_RS = `/// Parser B: Table-driven shift-reduce parser for simple arithmetic expressions.
///
/// Characteristics:
/// - Architecture: Bottom-up shift-reduce using an explicit operand and operator stack.
/// - Strengths: Strict linear O(n) performance, bounded constant stack memory, suitable for token streaming.
/// - Trade-offs: Less intuitive error recovery compared to recursive descent; harder to debug grammar states.
#[derive(Debug, PartialEq)]
pub enum AstNode {
    Val(i64),
    Op(char, Box<AstNode>, Box<AstNode>),
}

pub struct ShiftReduceParser;

impl ShiftReduceParser {
    pub fn parse(tokens: &[String]) -> Result<AstNode, String> {
        let mut values: Vec<AstNode> = Vec::new();
        let mut ops: Vec<char> = Vec::new();

        for tok in tokens {
            if let Ok(num) = tok.parse::<i64>() {
                values.push(AstNode::Val(num));
            } else if tok == "+" || tok == "-" {
                ops.push(tok.chars().next().unwrap());
            }
        }

        while let Some(op) = ops.pop() {
            if values.len() < 2 {
                return Err("Malformed token sequence".to_string());
            }
            let right = values.pop().unwrap();
            let left = values.pop().unwrap();
            values.push(AstNode::Op(op, Box::new(left), Box::new(right)));
        }

        values.pop().ok_or_else(|| "Empty tokens".to_string())
    }
}
`;

const UNUSED_RS = `// Unused functions module for scenario S4 (delegated audit)

pub fn dead_code_helper_alpha() -> &'static str {
    "never called anywhere in the codebase"
}

pub fn dead_code_helper_beta(multiplier: u32) -> u32 {
    multiplier * 42
}

pub fn actively_used_helper() -> i32 {
    100
}
`;

const SLOW_SH = `#!/usr/bin/env bash
# Slow-running command fixture for scenario S5
echo "slow job started: pid $$ at $(date)"
sleep 90
echo "slow job completed successfully at $(date)"
`;

/**
 * Creates a clean fixture repository at targetDir.
 * @param {string} targetDir
 */
export function createFixtureRepo(targetDir) {
  const resolved = path.resolve(targetDir);
  fs.mkdirSync(path.join(resolved, "src"), { recursive: true });

  fs.writeFileSync(path.join(resolved, "Cargo.toml"), CARGO_TOML, "utf8");
  fs.writeFileSync(path.join(resolved, "src/lib.rs"), LIB_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/math.rs"), MATH_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/cli.rs"), CLI_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/sort.rs"), SORT_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/parser_a.rs"), PARSER_A_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/parser_b.rs"), PARSER_B_RS, "utf8");
  fs.writeFileSync(path.join(resolved, "src/unused.rs"), UNUSED_RS, "utf8");

  const slowShPath = path.join(resolved, "slow.sh");
  fs.writeFileSync(slowShPath, SLOW_SH, "utf8");
  fs.chmodSync(slowShPath, 0o755);

  // Initialize git repository
  try {
    execFileSync("git", ["init"], { cwd: resolved, stdio: "ignore" });
    execFileSync("git", ["config", "user.name", "ulw-bench-fixture"], { cwd: resolved, stdio: "ignore" });
    execFileSync("git", ["config", "user.email", "ulw-bench@example.com"], { cwd: resolved, stdio: "ignore" });
    execFileSync("git", ["add", "."], { cwd: resolved, stdio: "ignore" });
    execFileSync("git", ["commit", "-m", "initial fixture commit"], { cwd: resolved, stdio: "ignore" });
  } catch (err) {
    // If git commands fail in a restricted environment, the fixture files still exist.
  }
}

/**
 * Checks whether the fixture tests pass (e.g. math.rs test is fixed).
 * @param {string} targetDir
 * @returns {{ pass: boolean, reason?: string }}
 */
export function checkFixtureTest(targetDir) {
  const mathRsPath = path.join(targetDir, "src/math.rs");
  if (!fs.existsSync(mathRsPath)) {
    return { pass: false, reason: "src/math.rs missing" };
  }
  const content = fs.readFileSync(mathRsPath, "utf8");
  if (content.includes("assert_eq!(add(2, 2), 5);")) {
    return { pass: false, reason: "assert_eq!(add(2, 2), 5) still present" };
  }
  if (content.includes("assert_eq!(add(2, 2), 4);") || !content.includes("assert_eq!(add(2, 2), 5);")) {
    return { pass: true };
  }
  return { pass: false, reason: "test_add not verified" };
}

function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args.includes("--help") || args.includes("-h")) {
    console.log("Usage: node fixture-generator.mjs <target-dir>");
    process.exit(0);
  }
  const targetDir = args[0];
  createFixtureRepo(targetDir);
  console.log(`Created ulw-bench fixture repository at: ${targetDir}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename || "")) {
  main();
}
