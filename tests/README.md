# Integration Tests for claude-sync

This directory contains comprehensive integration tests for the claude-sync tool.

## Test Files

### integration_sync_tests.rs

Comprehensive integration tests for the full push/pull/undo workflow using real test data from `/root/repos/claude-sync/data/`.

#### Test Coverage

1. **test_full_push_pull_cycle()**
   - Tests the complete sync workflow across two "machines"
   - Creates temporary directories for sync repo and fake Claude projects
   - Copies real test data to simulate Claude's project structure
   - Initializes git repository and performs initial push
   - Modifies conversations in the sync repo (simulating remote changes)
   - Pulls changes to another "machine" directory
   - Verifies files sync correctly and modifications are propagated
   - Uses 5 real conversation files from test data (60K+ total)

2. **test_undo_pull_restores_files()**
   - Tests the undo functionality for pull operations
   - Creates a snapshot before pull
   - Simulates pull modifying local files
   - Executes undo operation
   - Verifies files are restored to pre-pull state
   - Confirms operation history is updated correctly
   - Validates snapshot cleanup after successful undo

3. **test_undo_push_resets_git()**
   - Tests the undo functionality for push operations
   - Creates git repository with initial commit
   - Performs push operation (second commit)
   - Captures commit hash before push
   - Executes undo operation
   - Verifies git is reset to previous commit using soft reset
   - Confirms operation history is updated
   - Validates snapshot cleanup

4. **test_conflict_handling()**
   - Tests conflict detection and resolution
   - Simulates two machines with divergent changes
   - Both machines modify same conversation differently
   - One machine pushes to sync repo
   - Other machine attempts to pull
   - Verifies conflict is detected
   - Tests "keep both" resolution strategy
   - Confirms conflict file is created with timestamp suffix
   - Validates both versions are preserved

5. **test_operation_history_tracking()**
   - Tests comprehensive operation history management
   - Performs multiple push and pull operations
   - Verifies history contains all operations in correct order (most recent first)
   - Tests operation summaries and statistics
   - Validates operation type filtering
   - Tests persistence and reloading from disk
   - Confirms history rotation (max 5 operations)

6. **test_with_real_test_data()**
   - Uses actual test data files from `/root/repos/claude-sync/data/`
   - Validates parsing of real Claude Code conversation files
   - Tests with 5 different conversation files:
     - 4dd02356-9e88-4c94-a858-20da56e2c0d3.jsonl (13K)
     - 0f8c17e3-e10b-4468-9244-fadd93491883.jsonl (125 bytes, summary entry)
     - 39b62044-1070-44c6-bf52-6f51fccf3204.jsonl (1.6K)
     - 91f8d70f-6a64-453b-a9f2-d9dec6f2ed81.jsonl (31K)
     - 0a4d715a-f2cf-4dce-bd05-a8ad7c7c87f8.jsonl (15K)
   - Verifies session ID, entries, and content hash extraction
   - Tests write and re-read round-trip for data integrity

7. **test_snapshot_with_multiple_files()**
   - Tests snapshot functionality with multiple conversation files
   - Creates snapshots containing 3 different files
   - Modifies all files after snapshot creation
   - Restores snapshot and verifies all files are restored correctly
   - Tests snapshot serialization/deserialization
   - Validates binary data preservation

8. **test_concurrent_push_pull_operations()**
   - Tests operation history under rapid succession of operations
   - Simulates 10 consecutive push/pull operations
   - Verifies history rotation (keeps only last 5 operations)
   - Confirms most recent operations are preserved
   - Tests persistence across multiple save/load cycles

## Test Data

The tests use real Claude Code conversation data from `/root/repos/claude-sync/data/`:

### Directory: -tmp-test1/
- `4dd02356-9e88-4c94-a858-20da56e2c0d3.jsonl` - 13K conversation file with full conversation history including user messages, assistant responses, and file operations

### Directory: -tmp-tmp-Vm9PMxCP7R/
- `0f8c17e3-e10b-4468-9244-fadd93491883.jsonl` - 125 bytes summary entry
- `39b62044-1070-44c6-bf52-6f51fccf3204.jsonl` - 1.6K conversation file
- `91f8d70f-6a64-453b-a9f2-d9dec6f2ed81.jsonl` - 31K large conversation file
- `0a4d715a-f2cf-4dce-bd05-a8ad7c7c87f8.jsonl` - 15K conversation file

## Helper Functions

The test suite includes several helper functions to make tests more maintainable:

- **copy_test_data()** - Copies test data from `/root/repos/claude-sync/data/` to test directories
- **create_test_sync_state()** - Creates a mock sync state for isolated testing
- **create_test_filter_config()** - Creates default filter configuration
- **count_jsonl_files()** - Counts conversation files in a directory
- **discover_test_sessions()** - Discovers and parses all conversation sessions

## Running the Tests

```bash
# Run only integration sync tests
cargo test --test integration_sync_tests

# Run with output
cargo test --test integration_sync_tests -- --nocapture

# Run all tests
cargo test

# Run specific test
cargo test test_full_push_pull_cycle
```

## Test Isolation

All tests are properly isolated:
- Uses `tempfile::TempDir` for temporary directories
- Sets `HOME` environment variable to test config directory
- Each test uses its own temporary directories
- No side effects on user's actual `~/.claude` directory
- Tests can run in parallel safely
- Automatic cleanup after test completion

## Test Assertions

Tests include comprehensive assertions for:
- File existence and content
- Git commit hashes and repository state
- Operation history contents and ordering
- Snapshot creation and restoration
- Conflict detection and resolution
- Session parsing and data integrity
- Hash computation and comparison

## Dependencies

The tests use the following dependencies:
- `tempfile` - For temporary directory management
- `walkdir` - For recursive directory traversal
- `serde_json` - For JSON parsing and serialization
- `git2` - For git repository operations (indirect)

All dependencies are already included in the project's `Cargo.toml`.
