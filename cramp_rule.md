# CRAMP MCP Interaction Rules & Workflow Guide (LLM Specific)

**Goal**: To guide the LLM Agent in the precise and efficient use of the CRAMP toolset for analysing, fixing, and verifying Rust project tasks. You **MUST** strictly adhere to the following rules and workflow.

**Core Principle**: Trust the tool output, follow the workflow, pay strict attention to state management (active project, cache/ID invalidation), and prioritize the "Next Step" guidance returned by the tools. Client-side I/O (reading files, manual writes) is your responsibility.

---

## 🌟 Golden Rules - MUST Follow!

1.  **Workflow Driven**: You **MUST** strictly follow the standard workflow defined below. Do not skip steps; do not guess.
2.  **Follow Guidance**: Each tool's return value includes explicit `--- Next Step: ... ---` guidance. You **MUST** read and prioritize this guidance in your decision-making.
3.  **Cache & ID Invalidation Mechanism (CRITICAL!)**: State is cached; understand invalidation:
    *   `list_diagnostics`: Running this tool performs a fresh check, updates the diagnostic cache, and **invalidates ALL** previously generated `fix_id`s for the active project.
    *   `apply_fix`: Applying *any* fix **invalidates ALL** `fix_id`s AND the entire diagnostic cache for the active project. You **MUST** re-run `list_diagnostics` afterwards.
	*   `test_project`: Running tests **invalidates ALL** `fix_id`s AND the entire diagnostic cache for the active project.
    *   **Manual Client-Side Edit**: If your environment performs a file write operation outside of `apply_fix`, the server's cached diagnostics, fixes, and internal LSP state become stale. You **MUST** re-run `list_diagnostics` afterwards to resynchronise.
    *   **NEVER** attempt to use a `fix_id` after any operation that invalidates it (`list_diagnostics`, `apply_fix`, `test_project`, or any manual client-side edit).
4.  **0-Based Indexing (CRITICAL!)**:
   * All line/column numbers returned in JSON by tools (`list_diagnostics`, `list_document_symbols`, `search_workspace_symbols`) are **0-based**.
   * All line/column number parameters you provide to tools (`get_symbol_info`) **MUST** be **0-based**.
5.  **Relative Paths**: All parameters and return values involving `file_path` are paths relative to the current active project's root directory (e.g., `src/main.rs`). **NEVER** use absolute paths.
6.  **Active Project Context**: All tools, except `manage_projects`, operate exclusively on the currently **active project**. Ensure one is set and it is the correct one. `manage_projects` clears caches when switching projects.
7.  **Prioritize Auto-Fix**: The sequence `list_diagnostics` -> `get_code_actions` -> `apply_fix` is the preferred way to modify code. If `get_code_actions` provides a suitable fix (check description and `diff`), you **MUST prioritize** using `apply_fix` over attempting a manual client-side edit.
8. **Client-Side I/O Responsibility**:
    * The CRAMP tools do **not** include general-purpose `read_file` or `write_file` / `edit_file` tools (only `apply_fix` writes).
    * Your environment (the client) **MUST** provide the ability to read file content when needed for manual analysis.
	* Your environment (the client) **MUST** provide the ability to write/edit files when a manual fix (not `apply_fix`) is required. Remember Rule #3: manual edits require a subsequent `list_diagnostics` call.
9. **Exact Matching**: `get_code_actions` requires the *exact* `file_path` and `diagnostic_message` string as returned by the *most recent* `list_diagnostics` call.

---

## 🔄 The Mandatory Workflow

Your thoughts and actions must follow this flow: `Setup` -> `Diagnose` -> `Investigate/Act` -> `Verify` -> `Loop/Complete`.

```mermaid
graph TD
    Start --> 1-Manage["1. manage_projects (Get Snapshot)"];
     subgraph Cycle
     subgraph Diagnose
    2-ListDiag["2. list_diagnostics"];
	end
    1-Manage -- "Load/Set Active" --> 2-ListDiag;
    2-ListDiag -- "No Errors/Warnings" --> 6-Test;
    2-ListDiag -- "Has Errors/Warnings" --> 3-GetActions["3. get_code_actions<br>(for a specific diagnostic)"];

    3-GetActions --> 4-AutoFix{Has suitable<br>auto-fix (fix_id & diff)?};

    4-AutoFix -- "Yes" --> 4a-Apply["4a. apply_fix(fix_id)"];
	4a-Apply -- "Invalidates ALL Fixes & Diags" --> RerunList[Go Back to 2-ListDiag];

    4-AutoFix -- "No / Need Info" --> 5-ManualCycle;

     subgraph 5-ManualCycle [5. Investigation & Manual Fix]
         direction TB
		  5a-Info["get_symbol_info (API details)"]
		  5b-Symbols["list_document_symbols / search_workspace_symbols"]
          5c-Tree["get_file_tree (if needed)"]
          5d-Read["CLIENT: Read File Content"]
          5e-Write["CLIENT: Apply Manual Edit"]
          5a-Info --> 5b-Symbols
		  5b-Symbols --> 5c-Tree
		  5c-Tree --> 5d-Read
		  5d-Read --> 5e-Write
     end

    5-ManualCycle -- "AFTER Client Write (State is Stale)" --> RerunList;
	5-ManualCycle -- "Info gathering only (No Write)" --> 3-GetActions;

    subgraph Verify
    6-Test["6. test_project"];
	end
    6-Test -- "Tests PASS" --> END(Task Complete);
    6-Test -- "Tests FAIL (Invalidates ALL Fixes & Diags)" --> Analyze[Analyze test output];
    Analyze --> 5-ManualCycle;
    end

    %% Styling
     classDef critical fill:#f9f,stroke:#333,stroke-width:2px;
	 classDef client fill:#ccf,stroke:#333,stroke-width:1px;
     class 2-ListDiag,RerunList,4a-Apply,6-Test critical
	 class 5d-Read,5e-Write client

```

1.  🏁 **Setup: `manage_projects`**
    *   **Purpose**: Load a project, set the active project context, remove projects.
    *   **Output**: Text report including: workspace status, initial file tree snapshot, initial diagnostic summary, and "Next Step" guidance.
    *   **Rules**:
        *   **MUST** often be your first tool call.
        *   Use `project_path_or_name` with a filesystem path to load and activate.
        *   If a project is already loaded, provide its name to switch the active context (this also clears cache and provides a fresh snapshot).
        *   Check the returned status report to confirm the active project and review the initial snapshot.

2.  🧠 **Diagnose: `list_diagnostics`**
    *   **Purpose**: Run `cargo check` to get all project compilation errors and warnings. Populates the cache that `get_code_actions` reads from.
    *    **Output**: JSON list of diagnostics (`SimpleDiagnostic`) and "Next Step" guidance.
    *   **Rules**:
        *   Call after setting the active project (although `manage_projects` gives an initial summary).
        *   **MUST** be called *before* calling `get_code_actions`.
        *   **MUST** be re-called after *any* code modification (`apply_fix` or manual client-side edit) or after `test_project`.
        *   **Side-effect**: Invalidates all previously known `fix_id`s.
        *   Parse the JSON: Focus on `severity`, `message`, `file_path`, `line` (**0-based!**), `column` (**0-based!**).
        *   If the return indicates no errors, proceed to the **Verify** step (`test_project`).

3. **Find Fix: `get_code_actions`**
     *   **Purpose**: Find available automatic fixes for *one specific* diagnostic found by `list_diagnostics`.
     *   **Output**: JSON list of actions (`ActionWithId`), each with `id`, `description`, and `diff` preview, plus "Next Step" guidance.
     *    **Rules**:
         *   **MUST** only be called after `list_diagnostics`.
         *   Parameters `file_path` and `diagnostic_message` **MUST EXACTLY MATCH** an entry from the *most recent* `list_diagnostics` output.
         *   Review the `description` and `diff` to determine if a fix matches your goal.
         *   If suitable, record the `id` for use with `apply_fix`.
         *   If no suitable fixes, proceed to Step 5 (Investigation & Manual Fix).

4.  🛠️ **Act (Auto): `apply_fix`**
    *   **Purpose**: Apply an automatic fix identified by `get_code_actions`. Modifies files on disk.
    *   **Output**: Success message and "Next Step" guidance.
    *   **Priority**: High! If a suitable `fix_id` exists, you **MUST** prioritize using this tool over manual edits.
    *   **Rules**:
        *   The `fix_id` parameter **MUST** come from the result of `get_code_actions` called after the *most recent* `list_diagnostics`.
        *   **Side-effect**: Calling this tool invalidates all `fix_id`s AND the diagnostic cache.
        *   Upon success, you **MUST** return to step 2 (`list_diagnostics`).

5.  ✍️ **Act (Investigation & Manual): Explore & Edit**
    *    **Purpose**: Manually understand and modify code when no suitable auto-fix is available, or when tests fail.
    *    **Priority**: Lower than `apply_fix`.
    *    **Sub-steps & Tools**:
        *   **`get_symbol_info`**: Get API details (docs, signature, definition structure, methods, fields) for the code symbol. Provide `file_path` AND EITHER `line`/`column` (0-based) OR `symbol_name`. Output is Markdown.
		*   **`list_document_symbols`**: Get JSON list of symbols (structs, funcs, etc.) in a `file_path`. Locations are **0-based**.
		*   **`search_workspace_symbols`**: Find symbols matching `query` across the project. JSON list, locations are **0-based**.
        *   **`get_file_tree`**: Get text tree if the one from `manage_projects` is stale or insufficient.
        *    **CLIENT-SIDE: Read File Content**: Use your environment's capability to read code from `file_path` (relative) based on information from diagnostics or symbol tools.
        *    **CLIENT-SIDE: Apply Manual Edit**: Use your environment's capability to write/modify code.
            *   **Side-effect**: Makes server state (cache, LSP VFS) stale.
            *   Upon performing a client-side write, you **MUST** return to step 2 (`list_diagnostics`).

6.  ✅ **Verify: `test_project`**
    *   **Purpose**: Run `cargo test` for final confirmation.
    *   **Output**: Raw `cargo test` output (stdout/stderr) and "Next Step" guidance.
    *   **Rules**:
        *    **Side-effect**: Invalidates all `fix_id`s AND the diagnostic cache.
        *   Call this tool when `list_diagnostics` reports no errors or warnings.
        *   Carefully analyze the output:
            *   If all tests pass, the task is likely complete.
            *   If tests fail, analyze the failure information (which test, reason), then return to step 5 (Investigation & Manual Fix).
            * You can use the `test_name` parameter to run only a specific failing test, speeding up iteration.

7.  🎉 **Complete**:
     *  The task is considered complete if and only if `list_diagnostics` reports no errors AND `test_project` reports all tests passed.

---
## 🔧 Tool Detail Rules

*   **`manage_projects(project_path_or_name: Option<String>, remove_project_name: Option<String>)`**
    *   Always the first step.
    *   `project_path_or_name`: Path loads+activates; existing name only activates.
    *   `remove_project_name`: Removes project, shuts down its language server, clears cache.
    *   Returns initial snapshot (tree, diagnostics) and status (Text).
*   **`list_diagnostics(file_path: Option<String>, limit: Option<u32>)`**
    *   **Diagnostic Hub**. Runs `cargo check`.
    *   Output `line` and `column` are **0-based** (JSON).
    *   **Invalidates all previous `fix_id`s**.
    *   Must run before `get_code_actions`. Must re-run after any code change.
*    **`get_code_actions(file_path: String, diagnostic_message: String)`**
     *   Requires `file_path` and `diagnostic_message` **exactly** as returned by the most recent `list_diagnostics`.
     *   Output includes `id` and `diff` preview (JSON).
*   **`apply_fix(fix_id: String)`**
     *   `fix_id` **MUST** be from `get_code_actions` run after the most recent `list_diagnostics`.
     *   Modifies files on disk and notifies the internal LSP server.
     *   **Invalidates ALL `fix_id`s AND the diagnostic cache**.
     *   MUST re-run `list_diagnostics` after calling.
*   **`get_file_tree()`**
    *   Exploration. Output is a text tree with relative paths.
*    **`list_document_symbols(file_path: String)`**
     *   Exploration. `file_path` is relative.
     *   Output `line` and `column` are **0-based** (JSON).
*     **`get_symbol_info(file_path: String, line: Option<u32>, column: Option<u32>, symbol_name: Option<String>)`**
      *  API details.
      *  `file_path` is relative. Provide `file_path` AND EITHER `line`/`column` (0-based) OR `symbol_name`.
      *  Output is Markdown summary of docs, fields, methods etc.
*    **`search_workspace_symbols(query: String)`**
      * Exploration.
      * Output `line` and `column` are **0-based** (JSON).
*   **`test_project(test_name: Option<String>)`**
    *   Verification.
    *   Output is raw `cargo test` text. Carefully analyze success/failure messages.
     *   **Invalidates ALL `fix_id`s AND the diagnostic cache**.
    *   On failure, return to the manual fix cycle based on the output.
*   **CLIENT-SIDE I/O**
     * Not a CRAMP tool. Used for reading content and applying edits when `apply_fix` is not applicable.
	 * Any client-side *write* requires a follow-up call to `list_diagnostics`.

---
## 🚫 CRITICAL PITFALLS & Avoidance Strategies

*   **Pitfall 1**: Attempting to use a `fix_id` after it has been invalidated.
    *   **Invalidators**: `list_diagnostics`, `apply_fix`, `test_project`, any manual client-side edit.
    *   **Avoid**: Always follow the loop: ensure `fix_id` comes from `get_code_actions` called *immediately* after the *most recent* `list_diagnostics`, with no intervening invalidating operations. After `apply_fix` or manual edit, you MUST re-run `list_diagnostics`.
*   **Pitfall 2**: Calling `get_symbol_info` or interpreting JSON output using 1-based line/column numbers.
    *   **Avoid**: Always remember the interface is **0-based**.
*   **Pitfall 3**: Calling tools (except `manage_projects`) without an active project set.
    *   **Avoid**: Ensure `manage_projects` is run first and successfully sets an active project. Check its output.
*    **Pitfall 4**: `get_code_actions` provides a perfect auto-fix, but the Agent chooses the inefficient manual path (Client-Side Read -> Client-Side Write).
     *   **Avoid**: **Always** check `get_code_actions` description and `diff` first; prioritize using `apply_fix`.
*   **Pitfall 5**: Ignoring the "Next Step" guidance returned by the tools.
    *    **Avoid**: Treat the guidance text as one of the highest-priority inputs for your decision-making.
*   **Pitfall 6**: Declaring completion when `list_diagnostics` has no errors, but without running `test_project`.
    *   **Avoid**: Compilation success does not equal logical correctness. Verification via `test_project` is **mandatory** before completion.
*    **Pitfall 7**: Performing a manual client-side edit and then calling `get_code_actions` or `apply_fix` without first re-running `list_diagnostics`.
     *   **Avoid**: Manual edits make the server cache stale. You MUST re-run `list_diagnostics` immediately after any client-side write.
*     **Pitfall 8**: Calling `get_code_actions` with a `diagnostic_message` that does not exactly match the string from the last `list_diagnostics`.
      *    **Avoid**: Copy the message string precisely.

---
Strict adherence to these rules and the workflow will ensure the most efficient use of the CRAMP toolset.
