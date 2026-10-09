#!/bin/bash
# Generate test archive fixtures using Docker
# Run this once to create the test fixtures, then commit them to git.
#
# These fixtures have clearly "test only" names to avoid being flagged
# by any content scanning bots.
#
# Usage: Run from the kobload root directory:
#   ./api/tests/fixtures/generate_fixtures.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Generating test archive fixtures ==="
echo "Output directory: $SCRIPT_DIR"

# Use Alpine with p7zip
docker run --rm -v "$SCRIPT_DIR:/fixtures" -w /fixtures alpine:3.19 sh -c '
set -e

# Install tools
apk add --no-cache p7zip unrar-free zip >/dev/null 2>&1

echo "Creating test content..."
mkdir -p /tmp/work
echo "KOBLOAD_TEST_CONTENT_12345" > /tmp/work/kobload_test_file.txt

# =============================================================================
# Fixture 1: Nested split RAR inside ZIP
# Structure: test_kobload_nested_split.zip -> test_kobload.rar + test_kobload.r00 -> kobload_test_file.txt
# =============================================================================
echo "Creating fixture 1: nested split RAR inside ZIP..."
cd /tmp/work

# Create a split RAR (7z can create split archives, use .rar extension trick)
# Actually 7z cannot create real RAR files. We will create a 7z split and rename,
# but for true RAR testing we need a different approach.

# Alternative: Create a scenario we CAN test - 7z split inside ZIP
# This tests the numeric split code path which is equally important

# For the RAR test, we will create a mock structure that tests the PATH parsing
# and extraction flow, even if we cannot create a real split RAR without rar tool

# Lets create what we CAN: 7z splits and zip files

# =============================================================================
# Fixture 2: Numeric split archive inside ZIP (.7z.001, .7z.002)
# Structure: test_kobload_numeric_split.zip -> test_archive.7z.001 + .002 -> kobload_test_file.txt
# =============================================================================
echo "Creating fixture 2: numeric 7z split inside ZIP..."
rm -rf /tmp/numeric && mkdir -p /tmp/numeric
cd /tmp/numeric

# Create a 7z archive, then split it
7z a -mx0 test_archive.7z /tmp/work/kobload_test_file.txt >/dev/null

# Split into small chunks (we need at least 2 parts)
# The file is tiny, so we split at minimum size
split -b 50 -d --additional-suffix=.split test_archive.7z test_archive.7z.
# Rename to .001, .002 format
mv test_archive.7z.00.split test_archive.7z.001 2>/dev/null || true
mv test_archive.7z.01.split test_archive.7z.002 2>/dev/null || true
mv test_archive.7z.02.split test_archive.7z.003 2>/dev/null || true
rm -f test_archive.7z *.split 2>/dev/null || true

# Zip them up
zip -q /fixtures/test_kobload_numeric_split.zip test_archive.7z.*
echo "  Created: test_kobload_numeric_split.zip"

# =============================================================================
# Fixture 3: Distributed numeric split (volumes in separate ZIPs)
# Structure: test_kobload_dist_001.zip -> test_archive.7z.001
#            test_kobload_dist_002.zip -> test_archive.7z.002
# =============================================================================
echo "Creating fixture 3: distributed numeric split..."
cd /tmp/numeric
zip -q /fixtures/test_kobload_dist_001.zip test_archive.7z.001
zip -q /fixtures/test_kobload_dist_002.zip test_archive.7z.002
if [ -f test_archive.7z.003 ]; then
    zip -q /fixtures/test_kobload_dist_003.zip test_archive.7z.003
    echo "  Created: test_kobload_dist_001.zip, test_kobload_dist_002.zip, test_kobload_dist_003.zip"
else
    echo "  Created: test_kobload_dist_001.zip, test_kobload_dist_002.zip"
fi

# =============================================================================
# Fixture 4: Simple nested archive (ZIP inside ZIP)
# Structure: test_kobload_nested.zip -> inner.zip -> kobload_test_file.txt
# =============================================================================
echo "Creating fixture 4: simple nested ZIP..."
rm -rf /tmp/nested && mkdir -p /tmp/nested
cd /tmp/nested
zip -q inner.zip /tmp/work/kobload_test_file.txt
zip -q /fixtures/test_kobload_nested.zip inner.zip
echo "  Created: test_kobload_nested.zip"

# =============================================================================
# Fixture 5: Plain archive for baseline testing
# Structure: test_kobload_plain.zip -> kobload_test_file.txt
# =============================================================================
echo "Creating fixture 5: plain ZIP..."
zip -q /fixtures/test_kobload_plain.zip /tmp/work/kobload_test_file.txt
echo "  Created: test_kobload_plain.zip"

echo ""
echo "=== Fixture generation complete ==="
ls -la /fixtures/*.zip
'

echo ""
echo "Fixtures generated in: $SCRIPT_DIR"
echo "Commit these files to git for reproducible tests."
