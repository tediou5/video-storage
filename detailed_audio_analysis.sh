#!/bin/bash

# Detailed Audio Analysis Script for Video Storage Project
# This script performs segment-by-segment audio analysis to identify missing audio sections

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== Detailed Audio Analysis Tool ===${NC}"

# Check dependencies
for cmd in ffmpeg ffprobe bc; do
    if ! command -v "$cmd" &> /dev/null; then
        echo -e "${RED}Error: $cmd not found. Please install required tools.${NC}"
        exit 1
    fi
done

# Function to analyze audio in segments
analyze_audio_segments() {
    local file="$1"
    local segment_duration="${2:-5}" # Default 5 second segments

    echo -e "\n${YELLOW}=== Segment Analysis for: $file ===${NC}"
    echo -e "${CYAN}Segment duration: ${segment_duration}s${NC}"

    if [ ! -f "$file" ]; then
        echo -e "${RED}File not found: $file${NC}"
        return 1
    fi

    # Get total duration
    local total_duration=$(ffprobe -v quiet -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "$file" 2>/dev/null || echo "0")

    if (( $(echo "$total_duration <= 0" | bc -l) )); then
        echo -e "${RED}Could not determine file duration${NC}"
        return 1
    fi

    echo -e "${GREEN}Total duration: ${total_duration}s${NC}"

    # Extract complete audio track for analysis
    local temp_audio="/tmp/detailed_audio_$(basename "$file" .webm).wav"
    echo -e "${CYAN}Extracting audio to: $temp_audio${NC}"

    ffmpeg -i "$file" -vn -acodec pcm_s16le -ar 44100 -ac 2 "$temp_audio" -y 2>/dev/null || {
        echo -e "${RED}Failed to extract audio from $file${NC}"
        return 1
    }

    # Calculate number of segments
    local num_segments=$(echo "($total_duration + $segment_duration - 0.01) / $segment_duration" | bc -l | cut -d. -f1)

    echo -e "${GREEN}Analyzing $num_segments segments...${NC}"
    echo -e "${BLUE}Segment | Start Time | End Time   | RMS Level | Peak Level | Status${NC}"
    echo -e "${BLUE}--------|-----------|------------|-----------|------------|-------${NC}"

    local silent_segments=0
    local problematic_segments=()

    for ((i=0; i<num_segments; i++)); do
        local start_time=$(echo "$i * $segment_duration" | bc -l)
        local end_time=$(echo "($i + 1) * $segment_duration" | bc -l)

        # Ensure we don't go beyond file duration
        if (( $(echo "$end_time > $total_duration" | bc -l) )); then
            end_time=$total_duration
        fi

        local actual_duration=$(echo "$end_time - $start_time" | bc -l)

        # Skip if segment is too short
        if (( $(echo "$actual_duration < 0.1" | bc -l) )); then
            continue
        fi

        # Analyze this segment
        local analysis=$(ffmpeg -ss "$start_time" -i "$temp_audio" -t "$actual_duration" -af "volumedetect" -f null - 2>&1 | grep -E "(mean_volume|max_volume)" || echo "mean_volume: N/A dB max_volume: N/A dB")

        local rms_level=$(echo "$analysis" | grep "mean_volume" | awk '{print $5}' || echo "N/A")
        local peak_level=$(echo "$analysis" | grep "max_volume" | awk '{print $5}' || echo "N/A")

        # Determine status
        local status="${GREEN}OK${NC}"
        if [[ "$rms_level" == "N/A" ]] || [[ "$peak_level" == "N/A" ]]; then
            status="${RED}ERROR${NC}"
            problematic_segments+=($i)
        elif (( $(echo "$rms_level == -inf" | bc -l 2>/dev/null || echo "0") )) || [[ "$rms_level" == "-inf" ]]; then
            status="${YELLOW}SILENT${NC}"
            silent_segments=$((silent_segments + 1))
            problematic_segments+=($i)
        elif (( $(echo "$rms_level < -50" | bc -l 2>/dev/null || echo "0") )); then
            status="${YELLOW}QUIET${NC}"
        fi

        printf "%-7s | %-9.2f | %-10.2f | %-9s | %-10s | %s\n" \
               "$((i+1))" "$start_time" "$end_time" "$rms_level" "$peak_level" "$status"
    done

    echo -e "\n${YELLOW}=== Analysis Summary ===${NC}"
    echo -e "${GREEN}Total segments analyzed: $num_segments${NC}"
    echo -e "${YELLOW}Silent segments: $silent_segments${NC}"
    echo -e "${RED}Problematic segments: ${#problematic_segments[@]}${NC}"

    if [ ${#problematic_segments[@]} -gt 0 ]; then
        echo -e "\n${RED}Problematic segment details:${NC}"
        for seg in "${problematic_segments[@]}"; do
            local start_time=$(echo "$seg * $segment_duration" | bc -l)
            local end_time=$(echo "($seg + 1) * $segment_duration" | bc -l)
            if (( $(echo "$end_time > $total_duration" | bc -l) )); then
                end_time=$total_duration
            fi
            printf "  Segment %d: %.2f - %.2f seconds\n" "$((seg+1))" "$start_time" "$end_time"
        done

        # Check if problematic segments are at the end
        local last_segments_check=3
        local total_segments_to_check=$(echo "$num_segments - $last_segments_check" | bc)

        for seg in "${problematic_segments[@]}"; do
            if (( $(echo "$seg >= $total_segments_to_check" | bc -l) )); then
                echo -e "\n${RED}⚠️  AUDIO TRUNCATION DETECTED!${NC}"
                echo -e "${RED}   Missing audio in the last $last_segments_check segments of the video${NC}"
                echo -e "${YELLOW}   This confirms the issue: audio ends prematurely${NC}"
                break
            fi
        done
    fi

    # Additional detailed analysis for last 30 seconds
    echo -e "\n${CYAN}=== Detailed End-of-File Analysis ===${NC}"
    local eof_start=$(echo "$total_duration - 30" | bc -l)
    if (( $(echo "$eof_start < 0" | bc -l) )); then
        eof_start=0
    fi

    echo -e "${CYAN}Analyzing last 30 seconds (${eof_start}s - ${total_duration}s) in 1-second intervals:${NC}"

    local end_analysis_segments=$(echo "$total_duration - $eof_start" | bc -l | cut -d. -f1)
    for ((i=0; i<end_analysis_segments; i++)); do
        local seg_start=$(echo "$eof_start + $i" | bc -l)
        local seg_end=$(echo "$eof_start + $i + 1" | bc -l)

        if (( $(echo "$seg_end > $total_duration" | bc -l) )); then
            seg_end=$total_duration
        fi

        local seg_analysis=$(ffmpeg -ss "$seg_start" -i "$temp_audio" -t 1 -af "volumedetect" -f null - 2>&1 | grep "mean_volume" | awk '{print $5}' || echo "-inf")

        if [[ "$seg_analysis" == "-inf" ]] || [[ "$seg_analysis" == "N/A" ]]; then
            printf "  ${RED}%.0f-%.0fs: SILENT${NC}\n" "$seg_start" "$seg_end"
        else
            printf "  ${GREEN}%.0f-%.0fs: %s dB${NC}\n" "$seg_start" "$seg_end" "$seg_analysis"
        fi
    done

    # Cleanup
    rm -f "$temp_audio"

    echo -e "\n${BLUE}=== Analysis Complete ===${NC}"
}

# Function to compare waveforms
generate_waveform_comparison() {
    local file="$1"
    local output_dir="${2:-waveform_analysis}"

    echo -e "\n${YELLOW}=== Generating Waveform Analysis ===${NC}"

    mkdir -p "$output_dir"

    # Generate full waveform
    local waveform_full="$output_dir/$(basename "$file" .webm)_full_waveform.png"
    ffmpeg -i "$file" -filter_complex "showwavespic=s=1920x480:colors=blue" -frames:v 1 "$waveform_full" -y 2>/dev/null && {
        echo -e "${GREEN}Full waveform saved: $waveform_full${NC}"
    }

    # Generate last 30 seconds waveform
    local duration=$(ffprobe -v quiet -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "$file" 2>/dev/null || echo "0")
    local start_time=$(echo "$duration - 30" | bc -l)
    if (( $(echo "$start_time < 0" | bc -l) )); then
        start_time=0
    fi

    local waveform_end="$output_dir/$(basename "$file" .webm)_last30s_waveform.png"
    ffmpeg -ss "$start_time" -i "$file" -t 30 -filter_complex "showwavespic=s=1920x480:colors=red" -frames:v 1 "$waveform_end" -y 2>/dev/null && {
        echo -e "${GREEN}Last 30s waveform saved: $waveform_end${NC}"
    }
}

# Main function
main() {
    local input_file=""
    local segment_duration=5
    local generate_waveform=false
    local output_dir="waveform_analysis"

    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            -f|--file)
                input_file="$2"
                shift 2
                ;;
            -s|--segment-duration)
                segment_duration="$2"
                shift 2
                ;;
            -w|--waveform)
                generate_waveform=true
                shift
                ;;
            -o|--output-dir)
                output_dir="$2"
                shift 2
                ;;
            -h|--help)
                echo "Usage: $0 [OPTIONS]"
                echo "Options:"
                echo "  -f, --file FILE           Video file to analyze"
                echo "  -s, --segment-duration N  Segment duration in seconds (default: 5)"
                echo "  -w, --waveform           Generate waveform visualizations"
                echo "  -o, --output-dir DIR     Output directory for waveforms (default: waveform_analysis)"
                echo "  -h, --help               Show this help message"
                echo ""
                echo "Examples:"
                echo "  $0 -f video.webm"
                echo "  $0 -f video.webm -s 10 -w"
                echo "  $0 -f video.webm -s 1 -w -o analysis_results"
                exit 0
                ;;
            *)
                echo -e "${RED}Unknown option: $1${NC}"
                exit 1
                ;;
        esac
    done

    if [ -z "$input_file" ]; then
        echo -e "${RED}No input file specified. Use -f to specify a file.${NC}"
        echo "Use --help for usage information"
        exit 1
    fi

    # Perform analysis
    analyze_audio_segments "$input_file" "$segment_duration"

    if [ "$generate_waveform" = true ]; then
        generate_waveform_comparison "$input_file" "$output_dir"
    fi

    echo -e "\n${YELLOW}=== Recommendations ===${NC}"
    echo "1. If silent segments are detected at the end, check the audio flush logic"
    echo "2. Look for PTS discontinuities in the conversion logs"
    echo "3. Verify that audio encoder properly handles the last audio frames"
    echo "4. Check if the audio resampler is dropping samples during flush"
    echo "5. Consider adding padding to ensure complete audio processing"
}

main "$@"
