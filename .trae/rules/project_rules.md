# CRAMP MCP Interaction Rules & Workflow Guide (LLM Agent Specification)

**Primary Objective**: To provide comprehensive guidance for LLM agents in the precise, efficient, and reliable use of the CRAMP (Cargo Rust Analysis & Management Platform) toolset for analyzing, diagnosing, fixing, and verifying Rust project tasks. Strict adherence to these rules and workflows is **MANDATORY** for successful operation.

**Fundamental Principles**: 
- **Trust Tool Output**: Always rely on tool responses as authoritative sources of information
- **Follow Prescribed Workflow**: Execute the standardized workflow without deviation or shortcuts
- **State Management Vigilance**: Pay critical attention to active project context and cache/ID invalidation mechanisms
- **Guidance Priority**: Prioritize and act upon the "Next Step" guidance returned by each tool
- **Client I/O Responsibility**: Your environment (client) is responsible for file reading and manual writing operations (except `apply_fix`)

---

## 🌟 Golden Rules - MANDATORY COMPLIANCE!

### 1. **Workflow Adherence (NON-NEGOTIABLE)**
You **MUST** strictly follow the standardized workflow defined below without deviation, shortcuts, or assumptions. Each step serves a critical purpose in maintaining system integrity and achieving reliable results.

### 2. **Next Step Guidance Compliance**
Every tool response includes explicit `--- Next Step: ... ---` guidance. You **MUST** read, understand, and prioritize this guidance in your decision-making process. This guidance is algorithmically generated based on current system state and represents the optimal next action.

### 3. **Cache & ID Invalidation Mechanism (ABSOLUTELY CRITICAL!)**
The system employs sophisticated caching for performance optimization. Understanding and respecting invalidation rules is **ESSENTIAL** for correct operation:

**Invalidation Triggers:**
- **`list_diagnostics`**: Executes fresh `cargo check`, updates diagnostic cache, **invalidates ALL** previously generated `fix_id`s for the active project
- **`apply_fix`**: Applying *any* automatic fix **invalidates ALL** `fix_id`s AND the entire diagnostic cache for the active project. You **MUST** re-run `list_diagnostics` immediately afterward
- **`test_project`**: Running tests **invalidates ALL** `fix_id`s AND the entire diagnostic cache for the active project
- **Manual Client-Side Edits**: Any file write operation performed outside of `apply_fix` makes server cache stale. You **MUST** re-run `list_diagnostics` afterward to resynchronize

**Critical Prohibition**: **NEVER** attempt to use a `fix_id` after any invalidating operation (`list_diagnostics`, `apply_fix`, `test_project`, or manual client-side edit).

### 4. **0-Based Indexing System (CRITICAL!)**
- **Input**: All line/character number parameters you provide to tools (`get_symbol_info`) **MUST** be **0-based**
- **Output**: All line/character numbers returned in JSON by tools (`list_diagnostics`, `list_document_symbols`, `search_workspace_symbols`) are **0-based**
- **Consistency**: Maintain 0-based indexing throughout all operations to prevent off-by-one errors

### 5. **Relative Path Convention**
All parameters and return values involving `file_path` are paths relative to the current active project's root directory (e.g., `src/main.rs`, `tests/integration_test.rs`). **ABSOLUTELY NEVER** use absolute paths in tool parameters.

### 6. **Active Project Context Management**
All tools operate exclusively on the currently **active project**. You must:
- Ensure an active project is set before using other tools using `add_server` and `set_active_project`
- Verify you're operating on the correct project
- Remember that switching projects clears all caches

### 7. **Automated Fix Prioritization**
The sequence `list_diagnostics` → `get_code_actions` → `apply_fix` is the **preferred and optimized** method for code modification. If `get_code_actions` provides a suitable fix (verify description and `diff` preview), you **MUST prioritize** using `apply_fix` over manual client-side edits.

### 8. **Client-Side I/O Responsibility & Limitations**
- **CRAMP Limitation**: The toolset does **NOT** include general-purpose `read_file` or `write_file`/`edit_file` tools (only `apply_fix` performs writes)
- **Client Requirement**: Your environment **MUST** provide file reading capability for manual analysis
- **Client Requirement**: Your environment **MUST** provide file writing/editing capability for manual fixes
- **Critical Reminder**: Manual edits require subsequent `list_diagnostics` call (Rule #3)

### 9. **Exact String Matching Requirement**
`get_code_actions` requires **EXACT** `file_path` and `diagnostic_message` strings as returned by the *most recent* `list_diagnostics` call. Even minor variations (whitespace, punctuation) will cause failures.

---

## 🔄 Mandatory Workflow - STRICT ADHERENCE REQUIRED

### Phase 1: Comprehensive Diagnostic Assessment
**Objective**: Obtain complete picture of current project health

1. **`list_diagnostics`** - Execute fresh `cargo check` to get current compilation errors/warnings
   - **Critical**: This invalidates ALL previous `fix_id`s
   - **Analysis**: Review all diagnostics for severity and patterns
   - **Prioritization**: Focus on errors before warnings
   - **Next**: Proceed to investigation if diagnostics exist, or verification if clean

### Phase 2: Strategic Code Investigation (Conditional)
**Objective**: Gather necessary context for understanding and fixing issues

2. **`get_file_tree`** - Map project structure and identify key components
   - **Purpose**: Understand project organization and locate relevant files
   - **Focus**: Pay attention to source directories, test directories, and configuration files

3. **`list_document_symbols`** - Analyze symbols within specific problematic files
   - **Purpose**: Understand file-level structure (functions, structs, implementations)
   - **Strategy**: Target files mentioned in diagnostic messages

4. **`search_workspace_symbols`** - Locate symbols across the entire workspace
   - **Purpose**: Find definitions, implementations, and usages of problematic symbols
   - **Scope**: Use when diagnostics reference symbols not immediately visible

5. **`get_symbol_info`** - Obtain detailed information about specific symbols
   - **Purpose**: Get precise location, signature, and context of symbols
   - **Precision**: Use exact coordinates from previous symbol searches

### Phase 3: Automated Repair Execution (Preferred Path)
**Objective**: Apply automated fixes when available and suitable

6. **`get_code_actions`** - Query available automatic fixes for specific diagnostics
   - **Precision**: Use EXACT `file_path` and `diagnostic_message` from latest `list_diagnostics`
   - **Evaluation**: Review fix descriptions and diff previews carefully
   - **Decision**: Proceed only if fix addresses the root cause appropriately

7. **`apply_fix`** - Execute the selected automatic fix
   - **Critical**: This invalidates ALL `fix_id`s and diagnostic cache
   - **Immediate Action**: Must run `list_diagnostics` immediately after
   - **Verification**: Confirm the fix was applied successfully

### Phase 4: Manual Repair Implementation (Fallback Path)
**Objective**: Address issues when automated fixes are unavailable or unsuitable

8. **Manual Code Modification** - Use your environment's file editing capabilities
   - **Approach**: Apply targeted fixes based on diagnostic analysis and code investigation
   - **Best Practices**: Make minimal, focused changes that address root causes
   - **Documentation**: Consider adding comments explaining complex fixes

9. **`list_diagnostics`** - Mandatory re-check after manual changes
    - **Purpose**: Cache invalidation and fresh diagnostic assessment
    - **Critical**: This step is NON-OPTIONAL after any manual edit
    - **Analysis**: Verify manual changes resolved intended issues without introducing new ones

### Phase 5: Comprehensive Verification & Validation
**Objective**: Ensure fixes are complete, correct, and don't introduce regressions

10. **`test_project`** - Execute project test suite
    - **Purpose**: Verify fixes don't break existing functionality
    - **Critical**: This invalidates ALL `fix_id`s and diagnostic cache
    - **Analysis**: Review test results for any new failures or regressions

11. **`list_diagnostics`** - Final diagnostic verification
    - **Purpose**: Confirm all targeted issues are resolved
    - **Success Criteria**: Zero errors, minimal warnings
    - **Completion**: Project is ready for use when diagnostics are clean

### Workflow Success Criteria
- **Zero compilation errors** in final `list_diagnostics`
- **All tests passing** in `test_project`
- **No new issues introduced** during the repair process
- **Clean project state** ready for continued development

---

## 🎯 Summary: Your Mission

As an LLM agent working with CRAMP, your mission is to:

1. **Maintain System Integrity**: Respect cache invalidation rules and follow the mandatory workflow without deviation
2. **Prioritize Automation**: Use `apply_fix` whenever suitable automatic fixes are available
3. **Ensure Completeness**: Never leave a project in a broken state; always verify fixes through testing
4. **Follow Guidance**: Trust and act upon the "Next Step" guidance provided by each tool
5. **Communicate Clearly**: Report your progress, findings, and any limitations encountered

**Remember**: CRAMP is designed to make Rust development more efficient and reliable. By following these rules strictly, you become a powerful ally in maintaining code quality and resolving issues systematically.

**Your workflow mantra**: `Diagnose` → `Investigate/Act` → `Verify` → `Loop/Complete`

```mermaid
graph TD
    Start --> 1-ListDiag["1. list_diagnostics"];
     subgraph Cycle
     subgraph Diagnose
    1-ListDiag["1. list_diagnostics"];
	end
    1-ListDiag -- "No Errors/Warnings" --> 4-Test;
    1-ListDiag -- "Has Errors/Warnings" --> 2-GetActions["2. get_code_actions<br>(for a specific diagnostic)"];

    2-GetActions --> 3-AutoFix{Has suitable<br>auto-fix (fix_id & diff)?};

    3-AutoFix -- "Yes" --> 3a-Apply["3a. apply_fix(fix_id)"];
	3a-Apply -- "Invalidates ALL Fixes & Diags" --> RerunList[Go Back to 1-ListDiag];

    3-AutoFix -- "No / Need Info" --> 4-ManualCycle;

     subgraph 4-ManualCycle [4. Investigation & Manual Fix]
         direction TB
		  3a-Info["get_symbol_info (API details)"]
		  3b-Symbols["list_document_symbols / search_workspace_symbols"]
          3c-Tree["get_file_tree (if needed)"]
          3d-Read["CLIENT: Read File Content"]
          3e-Write["CLIENT: Apply Manual Edit"]
          3a-Info --> 3b-Symbols
		  3b-Symbols --> 3c-Tree
		  3c-Tree --> 3d-Read
		  3d-Read --> 3e-Write
     end

    4-ManualCycle -- "AFTER Client Write (State is Stale)" --> RerunList;
	4-ManualCycle -- "Info gathering only (No Write)" --> 2-GetActions;

    subgraph Verify
    5-Test["5. test_project"];
	end
    5-Test -- "Tests PASS" --> END(Task Complete);
    5-Test -- "Tests FAIL (Invalidates ALL Fixes & Diags)" --> Analyze[Analyze test output];
    Analyze --> 4-ManualCycle;
    end

    %% Styling
     classDef critical fill:#f9f,stroke:#333,stroke-width:2px;
	 classDef client fill:#ccf,stroke:#333,stroke-width:1px;
     class 1-ListDiag,RerunList,3a-Apply,5-Test critical
	 class 3d-Read,3e-Write client

```

1.  🧠 **Diagnose: `list_diagnostics`**
    *   **Purpose**: Run `cargo check` to get all project compilation errors and warnings. Populates the cache that `get_code_actions` reads from.
    *    **Output**: JSON list of diagnostics (`SimpleDiagnostic`) and "Next Step" guidance.
    *   **Rules**:
        *   Call after setting the active project.
        *   **MUST** be called *before* calling `get_code_actions`.
        *   **MUST** be re-called after *any* code modification (`apply_fix` or manual client-side edit) or after `test_project`.
        *   **Side-effect**: Invalidates all previously known `fix_id`s.
        *   Parse the JSON: Focus on `severity`, `message`, `file_path`, `line` (**0-based!**), `character` (**0-based!**).
        *   If the return indicates no errors, proceed to the **Verify** step (`test_project`).

2. **Find Fix: `get_code_actions`**
     *   **Purpose**: Find available automatic fixes for *one specific* diagnostic found by `list_diagnostics`.
     *   **Output**: JSON list of actions (`ActionWithId`), each with `id`, `description`, and `diff` preview, plus "Next Step" guidance.
     *    **Rules**:
         *   **MUST** only be called after `list_diagnostics`.
         *   Parameters `file_path` and `diagnostic_message` **MUST EXACTLY MATCH** an entry from the *most recent* `list_diagnostics` output.
         *   Review the `description` and `diff` to determine if a fix matches your goal.
         *   If suitable, record the `id` for use with `apply_fix`.
         *   If no suitable fixes, proceed to Step 5 (Investigation & Manual Fix).

3.  🛠️ **Act (Auto): `apply_fix`**
    *   **Purpose**: Apply an automatic fix identified by `get_code_actions`. Modifies files on disk.
    *   **Output**: Success message and "Next Step" guidance.
    *   **Priority**: High! If a suitable `fix_id` exists, you **MUST** prioritize using this tool over manual edits.
    *   **Rules**:
        *   The `fix_id` parameter **MUST** come from the result of `get_code_actions` called after the *most recent* `list_diagnostics`.
        *   **Side-effect**: Calling this tool invalidates all `fix_id`s AND the diagnostic cache.
        *   Upon success, you **MUST** return to step 1 (`list_diagnostics`).

4.  ✍️ **Act (Investigation & Manual): Explore & Edit**
    *    **Purpose**: Manually understand and modify code when no suitable auto-fix is available, or when tests fail.
    *    **Priority**: Lower than `apply_fix`.
    *    **Sub-steps & Tools**:
        *   **`get_symbol_info`**: Get API details (docs, signature, definition structure, methods, fields) for the code symbol. Provide `file_path` AND EITHER `line`/`character` (0-based) OR `symbol_name`. Output is Markdown.
		*   **`list_document_symbols`**: Get JSON list of symbols (structs, funcs, etc.) in a `file_path`. Locations are **0-based**.
		*   **`search_workspace_symbols`**: Find symbols matching `query` across the project. JSON list, locations are **0-based**.
        *   **`get_file_tree`**: Get text tree to understand project structure.
        *    **CLIENT-SIDE: Read File Content**: Use your environment's capability to read code from `file_path` (relative) based on information from diagnostics or symbol tools.
        *    **CLIENT-SIDE: Apply Manual Edit**: Use your environment's capability to write/modify code.
            *   **Side-effect**: Makes server state (cache, LSP VFS) stale.
            *   Upon performing a client-side write, you **MUST** return to step 1 (`list_diagnostics`).

5.  ✅ **Verify: `test_project`**
    *   **Purpose**: Run `cargo test` for final confirmation.
    *   **Output**: Raw `cargo test` output (stdout/stderr) and "Next Step" guidance.
    *   **Rules**:
        *    **Side-effect**: Invalidates all `fix_id`s AND the diagnostic cache.
        *   Call this tool when `list_diagnostics` reports no errors or warnings.
        *   Carefully analyze the output:
            *   If all tests pass, the task is likely complete.
            *   If tests fail, analyze the failure information (which test, reason), then return to step 4 (Investigation & Manual Fix).
            * You can use the `test_name` parameter to run only a specific failing test, speeding up iteration.

6.  🎉 **Complete**:
     *  The task is considered complete if and only if `list_diagnostics` reports no errors AND `test_project` reports all tests passed.

---
## 🔧 Tool Detail Rules

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
*   **`list_document_symbols(file_path: String)`**
     *   Exploration. `file_path` is relative.
     *   Output `line` and `character` are **0-based** (JSON).
*     **`get_symbol_info(file_path: String, line: Option<u32>, character: Option<u32>, symbol_name: Option<String>)`**
      *  API details.
      *  `file_path` is relative. Provide `file_path` AND EITHER `line`/`character` (0-based) OR `symbol_name`.
      *  Output is Markdown summary of docs, fields, methods etc.
*    **`search_workspace_symbols(query: String)`**
      * Exploration.
      * Output `line` and `character` are **0-based** (JSON).
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
*   **Pitfall 2**: Calling `get_symbol_info` or interpreting JSON output using 1-based line/character numbers.
    *   **Avoid**: Always remember the interface is **0-based**.
*   **Pitfall 3**: Calling tools when no active project is available.
    *   **Avoid**: Ensure the system has an active project context before using CRAMP tools. This is typically handled by the client/server setup.
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
