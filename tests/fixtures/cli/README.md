# CLI contract fixtures

Each JSON file describes one scenario from the original CLI suite. A step
applies changes to the candidate tree (`$TREE0`) or base tree (`$TREE1`), runs
the real binary, and compares its exit status and output with the expected
result. The two trees start empty. A null file entry deletes the path. Large
inputs use repeated text or a sparse zero-filled file. JSON output is
compared structurally, and text output is compared as written after removing
temporary directory names. Keep hand-scored expectations when updating or
adding syntax cases.
