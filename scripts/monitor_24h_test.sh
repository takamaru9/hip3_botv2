#!/bin/bash
# 24-hour test monitoring script
# Records errors to bug directory

LOG_DIR="/Users/taka/crypto_trading_bot/hip3_botv2/logs"
BUG_DIR="/Users/taka/crypto_trading_bot/hip3_botv2/bug"
LOG_FILE=$(ls -t $LOG_DIR/24h_test_*.log 2>/dev/null | head -1)

if [ -z "$LOG_FILE" ]; then
    echo "No log file found"
    exit 1
fi

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$BUG_DIR/error_report_${TIMESTAMP}.md"

# Check process status
PID=$(pgrep -f "hip3-bot.*mainnet")
if [ -z "$PID" ]; then
    echo "# Process Crash Detected" > "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "**Time**: $(date)" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "## Last 200 lines of log:" >> "$OUTPUT_FILE"
    echo "\`\`\`" >> "$OUTPUT_FILE"
    tail -200 "$LOG_FILE" >> "$OUTPUT_FILE"
    echo "\`\`\`" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "Process hip3-bot crashed or stopped unexpectedly"
    exit 1
fi

# Check for errors
ERROR_COUNT=$(grep -c "ERROR\|panic\|FATAL" "$LOG_FILE" 2>/dev/null | tr -d '\n' || echo "0")
SIGNAL_COUNT=$(grep -c "Signal detected" "$LOG_FILE" 2>/dev/null | tr -d '\n' || echo "0")

echo "=== 24h Test Status ==="
echo "Time: $(date)"
echo "PID: $PID"
echo "Log: $LOG_FILE"
echo "Signals: $SIGNAL_COUNT"
echo "Errors: $ERROR_COUNT"

if [ "$ERROR_COUNT" -gt 0 ]; then
    echo ""
    echo "# Error Report" > "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "**Time**: $(date)" >> "$OUTPUT_FILE"
    echo "**Log File**: $LOG_FILE" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "## Errors Found" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "\`\`\`" >> "$OUTPUT_FILE"
    grep -n "ERROR\|panic\|FATAL" "$LOG_FILE" >> "$OUTPUT_FILE"
    echo "\`\`\`" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    echo "## Context (lines around errors)" >> "$OUTPUT_FILE"
    echo "" >> "$OUTPUT_FILE"
    grep -B5 -A5 "ERROR\|panic\|FATAL" "$LOG_FILE" >> "$OUTPUT_FILE"
    echo ""
    echo "Error report saved to: $OUTPUT_FILE"
fi

# Check for WebSocket disconnections
WS_DISCONNECTS=$(grep -c "WebSocket closed\|WebSocket connection error\|Reconnecting" "$LOG_FILE" 2>/dev/null | tr -d '\n' || echo "0")
echo "WS Disconnects: $WS_DISCONNECTS"

# Log file size
LOG_SIZE=$(du -h "$LOG_FILE" | cut -f1)
echo "Log Size: $LOG_SIZE"
