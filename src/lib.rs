use std::io;

// If _not_ using PidNewProcess you must configure an executable to start with
// heap logging by creating a registry entry like:
// [HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\svchost.exe]
// "TracingFlags"=dword:00000001
//
// First enable the NT kernel logger, without this you don't get module/process resolution:
// xperf -on base
//
// Start a capture with:
// xperf -start heapsession -heap -pids 0 -stackwalk HeapAlloc+HeapRealloc+HeapCreate+HeapDestroy+HeapFree
// - or -
// xperf -start heapsession -heap -PidNewProcess notepad.exe -stackwalk HeapAlloc+HeapRealloc+HeapCreate+HeapDestroy+HeapFree
//
// Stop capture with (the two stops are intentional, one stops kernel, one stops user):
// xperf -stop -stop heapsession -d trace.etl
//
// Pretty-print log with (this output is what we parse):
// xperf -i trace.etl
// -or-
// xperf -i trace.etl -symbols

// These are the expected formats for the different events we care about
// make sure they match so we do not end up parsing the wrong fields
const HEAPCREATE_FORMAT:  &[&str] = &["HeapCreate", "TimeStamp", "Process Name ( PID)", "ThreadID", "HeapHandle", "Flags", "ReserveSize", "CommitSize", "AllocatedSize"];
const HEAPDESTROY_FORMAT: &[&str] = &["HeapDestroy", "TimeStamp", "Process Name ( PID)", "ThreadID", "HeapHandle"];
const HEAPALLOC_FORMAT:   &[&str] = &["HeapAlloc", "TimeStamp", "Process Name ( PID)", "ThreadID", "HeapHandle", "Address", "Size", "Source"];
const HEAPFREE_FORMAT:    &[&str] = &["HeapFree", "TimeStamp", "Process Name ( PID)", "ThreadID", "HeapHandle", "Address", "__Reserved", "Source"];
const HEAPREALLOC_FORMAT: &[&str] = &["HeapRealloc", "TimeStamp", "Process Name ( PID)", "ThreadID", "HeapHandle", "NewAddress", "OldAddress", "NewSize", "OldSize", "Source"];
const STACK_FORMAT:       &[&str] = &["Stack", "TimeStamp", "ThreadID", "No.", "Address", "Image!Function"];

#[derive(Clone, Debug)]
pub struct Stack(pub Vec<String>);

#[derive(Clone, Copy, Debug)]
pub enum HeapAction {
    Create  { heap: u64 },
    Destroy { heap: u64 },
    Alloc   { heap: u64, address: u64, size: u64 },
    Free    { heap: u64, address: u64 },
    Realloc { heap: u64, new_address: u64, old_address: u64, new_size: u64, old_size: u64 },
}

/// Parse a hex number as a string with an 0x prefix
fn parse_hex(string: &str) -> u64 {
    assert!(&string[..2] == "0x", "Invalid hex prefix");
    u64::from_str_radix(&string[2..], 16).expect("Invalid hex number")
}

/// Parses heap activity output from a log generated by `xperf -i user.etl`
pub fn parse(filename: &str) -> io::Result<Vec<(HeapAction, Stack)>> {
    // Read the whole file into memory
    // We don't read directly to string because sometimes we get some wonky
    // byte in the file, so we want to do lossy UTF-8 conversion
    let contents_raw = std::fs::read(filename)?;
    let contents     = String::from_utf8_lossy(&contents_raw);

    // Variables to track that we are using the right version of the file
    // format that we expect to parse
    let mut heapcreate_matches  = false;
    let mut heapdestroy_matches = false;
    let mut heapalloc_matches   = false;
    let mut heapfree_matches    = false;
    let mut heaprealloc_matches = false;
    let mut stack_matches       = false;
    let mut ready_to_parse      = false;

    let mut stacks       = Vec::new();
    let mut active_stack = Stack(Vec::new());

    // Create the list of activity that we return
    let mut activity = Vec::new();

    for line in contents.lines() {
        // Split by commas, trim whitespace, and collect this into a list
        let mut columns: Vec<&str> =
            line.split(",").map(|x| x.trim()).collect();

        // Skip empty rows
        if columns.len() == 0 { continue; }

        if !ready_to_parse {
            // Make sure we match on all the formats we expect
            if columns == HEAPALLOC_FORMAT {
                heapalloc_matches = true;
            } else if columns == HEAPFREE_FORMAT {
                heapfree_matches = true;
            } else if columns == HEAPREALLOC_FORMAT {
                heaprealloc_matches = true;
            } else if columns == HEAPCREATE_FORMAT {
                heapcreate_matches = true;
            } else if columns == HEAPDESTROY_FORMAT {
                heapdestroy_matches = true;
            } else if columns == STACK_FORMAT {
                stack_matches = true;
            } else if columns == &["EndHeader"] {
                assert!(heapalloc_matches && heapfree_matches &&
                    heaprealloc_matches && heapcreate_matches &&
                    heapdestroy_matches && stack_matches,
                    "Did not get expected headers");
                ready_to_parse = true;
            }
        } else {
            // Actual parsing of columns
            match columns[0] {
                "HeapCreate" => {
                    let heap = parse_hex(columns[4]);
                    activity.push(HeapAction::Create { heap });
                }
                "HeapDestroy" => {
                    let heap = parse_hex(columns[4]);
                    activity.push(HeapAction::Destroy { heap });
                }
                "HeapAlloc" => {
                    let heap    = parse_hex(columns[4]);
                    let address = parse_hex(columns[5]);
                    let size    = parse_hex(columns[6]);
                    activity.push(HeapAction::Alloc {
                        heap, address, size
                    });
                }
                "HeapFree" => {
                    let heap    = parse_hex(columns[4]);
                    let address = parse_hex(columns[5]);
                    activity.push(HeapAction::Free { heap, address });
                }
                "HeapRealloc" => {
                    let heap        = parse_hex(columns[4]);
                    let new_address = parse_hex(columns[5]);
                    let old_address = parse_hex(columns[6]);
                    let new_size    = parse_hex(columns[7]);
                    let old_size    = parse_hex(columns[8]);
                    activity.push(HeapAction::Realloc {
                        heap, new_address, old_address, new_size, old_size
                    });
                }
                "Stack" => {
                    let depth: u64 = columns[3].parse().unwrap();
                    let _address    = parse_hex(columns[4]);
                    let symbol     = columns[5];

                    // Reset stack if depth is 1
                    if depth == 1 {
                        // Save old stack
                        if active_stack.0.len() > 0 {
                            stacks.push(active_stack.clone());
                        }

                        // Clear stack
                        active_stack.0.clear();
                    }

                    // Push entry onto the stack
                    active_stack.0.push(symbol.into());
                }
                _ => {}
            }
        }
    }

    // Save active stack if one exists
    if active_stack.0.len() > 0 {
        stacks.push(active_stack);
    }

    // Make sure each activity has a stack
    assert!(activity.len() == stacks.len());

    // Join stacks to their activities
    Ok(activity.iter().cloned().zip(stacks).collect())
}