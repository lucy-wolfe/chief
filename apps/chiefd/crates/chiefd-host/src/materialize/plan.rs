//! The Pi builtin tool floor.
//!
//! # TOMBSTONE — the per-person PLAN, and the catalog it resolved against
//!
//! This module was `planPerson`: it walked four skill roots and two extension
//! roots to build a `Catalog`, resolved every person's declared
//! skills/extensions/packages against it, refused unknown, ambiguous,
//! duplicated, forbidden and tool-shadowing references, and produced a
//! `PersonPlan` the materializer then copied onto disk.
//!
//! Nothing is copied any more. Skills reach an agent through ONE symlink to the
//! company's own `.pi/skills`, and Pi — which already parses frontmatter,
//! enforces the Agent Skills name rules, dedupes by realpath and reports
//! collisions with a winner and a loser — owns every question this module
//! re-implemented. So `Catalog`, `CatalogRoots`, `PersonPlan`, `ResourceRef`,
//! `resolve_resource`, `skill_name`, `collapse_selected_resources`,
//! `is_forbidden_launcher_resource`, `expected_materialized_skill_names`,
//! `without_redundant_baseline_skills` and
//! `validate_organization_launch_resources` are all deleted, together with the
//! refusal codes that named their failures.
//!
//! `ORGANIZATION_PROTECTED_TOOL_NAMES` went with them, and its subject is worth
//! stating: it was the VALIDATION table for "a person-selected extension may
//! not shadow an organization tool". A person selects no extension that reaches
//! a pane, so there is nothing left that could shadow one, and a table pinned
//! against a rule nobody can break is a guard with no subject.
//!
//! `organization_person_tool_names` went too. It answered
//! `person_plan(person, catalog)?.tools` — the grant, VALIDATED against the
//! catalog. With no catalog it became a byte-for-byte synonym for
//! [`crate::converge_apply::resource_catalog::person_tool_names`], which is the
//! grant's authority and has always been where the tables live. Two names for
//! one derivation is how the two drift.
//!
//! # What survives, and why it survives HERE
//!
//! One constant. [`BUILTIN_TOOLS`] is the floor every person receives
//! unconditionally, and `packages/piing/test/RustToolListParity.test.ts` pins
//! it against the TypeScript `CapabilityPolicy.BUILTIN_TOOLS` **by this file's
//! path**. It is composed by `resource_catalog::person_tool_names` rather than
//! defaulted into a person row, so a seed that names one tool cannot cost a
//! person the other six — the defect that left a live company with 23 people
//! who could not open a file.

/// `BUILTIN_TOOLS` (`packages/piing/src/policy/CapabilityPolicy.ts:11-19`): the
/// tool floor every person receives without any resource providing it.
pub const BUILTIN_TOOLS: [&str; 7] = ["read", "bash", "edit", "write", "grep", "find", "ls"];
