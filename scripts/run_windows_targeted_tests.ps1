$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$target = 'x86_64-pc-windows-msvc'
$tests = @(
    @{ Package = 'jcode-base'; Name = 'command_candidates_adds_extension_on_windows' },
    @{ Package = 'jcode-base'; Name = 'command_exists_for_known_binary' },
    @{ Package = 'jcode-base'; Name = 'command_exists_absolute_path' },
    @{ Package = 'jcode-app-core'; Name = 'sibling_socket_path_roundtrip' },
    @{ Package = 'jcode-app-core'; Name = 'cleanup_socket_pair_removes_main_and_debug_files' },
    @{ Package = 'jcode-base'; Name = 'is_process_running_reports_exited_children_as_stopped' },
    @{ Package = 'jcode-base'; Name = 'spawn_replacement_process_returns_without_waiting_for_child_exit' },
    @{ Package = 'jcode-app-core'; Name = 'build_shell_command_uses_cmd_and_executes_command' },
    @{ Package = 'jcode-base'; Name = 'pipe_name_is_stable_and_normalizes_case_and_separators' },
    @{ Package = 'jcode-base'; Name = 'pipe_name_falls_back_when_stem_is_empty' },
    @{ Package = 'jcode-base'; Name = 'busy_pipe_is_reported_as_a_live_socket_path' },
    @{ Package = 'jcode-base'; Name = 'stream_pair_round_trips_bytes' },
    @{ Package = 'jcode-base'; Name = 'split_stream_supports_concurrent_read_and_write' },
    @{ Package = 'jcode'; Name = 'auto_provider_noninteractive_skips_untrusted_external_auth_instead_of_blocking' },
    @{ Package = 'jcode-tui'; Name = 'test_cancel_command_idle_reports_nothing_to_cancel' },
    @{ Package = 'jcode-tui'; Name = 'test_menu_number_rejected_as_api_key' },
    @{ Package = 'jcode-tui'; Name = 'test_command_palette_suppressed_while_api_key_prompt_pending' },
    @{ Package = 'jcode-tui'; Name = 'test_ctrl_c_with_active_copy_selection_copies_instead_of_quitting' },
    @{ Package = 'jcode-tui'; Name = 'test_ctrl_c_in_copy_mode_without_selection_still_falls_through' }
)

# Compile each exact package test binary outside the per-test timeout. Cold MSVC
# linking can exceed five minutes, while subsequent filtered executions are fast.
$packages = $tests | ForEach-Object { $_.Package } | Sort-Object -Unique
foreach ($package in $packages) {
    & cargo test --locked --target $target -p $package --lib --no-run
    if ($LASTEXITCODE -ne 0) {
        throw "Windows targeted test compilation failed for $package"
    }
}

foreach ($test in $tests) {
    & "$PSScriptRoot/invoke_cargo_with_timeout.ps1" `
        -Name "Windows targeted test: $($test.Name)" `
        -TimeoutSeconds 300 `
        -RequireTests `
        -CargoArgs @('test', '--locked', '--target', $target, '-p', $test.Package, '--lib', $test.Name, '--', '--nocapture')
}
