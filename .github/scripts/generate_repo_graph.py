#!/usr/bin/env python3
"""
Generate a beautiful, modern Tokyo-Night SVG commit activity graph
specifically for this Git repository's history.
"""

import os
import sys
import subprocess
from datetime import datetime, timedelta
from collections import Counter


def get_commit_dates():
    """Extract all commit dates (YYYY-MM-DD) from git history."""
    try:
        output = subprocess.check_output(
            ["git", "log", "--pretty=format:%ad", "--date=short"],
            encoding="utf-8",
            errors="replace"
        )
        dates = [line.strip() for line in output.splitlines() if line.strip()]
        return dates
    except Exception as e:
        print(f"Error reading git log: {e}", file=sys.stderr)
        return []


def generate_svg(dates, output_path):
    total_commits = len(dates)
    counts = Counter(dates)

    # Use today's date (or last commit date if available)
    if dates:
        try:
            latest_commit_date = datetime.strptime(dates[0], "%Y-%m-%d")
        except ValueError:
            latest_commit_date = datetime.now()
    else:
        latest_commit_date = datetime.now()

    today = max(datetime.now(), latest_commit_date)
    
    # 30-day window
    days_count = 30
    start_date = today - timedelta(days=days_count - 1)
    
    day_series = []
    for i in range(days_count):
        d = start_date + timedelta(days=i)
        d_str = d.strftime("%Y-%m-%d")
        day_series.append((d, counts.get(d_str, 0)))

    max_val = max(max(val for _, val in day_series), 1)
    # Give a bit of headroom
    y_max = max(max_val + 1, 4)

    width = 880
    height = 290
    plot_x = 55
    plot_y = 75
    plot_w = 780
    plot_h = 145
    base_y = plot_y + plot_h

    # Map points to coordinates
    points = []
    for i, (date_obj, val) in enumerate(day_series):
        x = plot_x + (i / (days_count - 1)) * plot_w
        # y goes from base_y down to plot_y
        y = base_y - (val / y_max) * plot_h
        points.append((x, y, date_obj, val))

    # Build smooth cubic bezier path
    if len(points) > 1:
        line_path = f"M {points[0][0]:.1f} {points[0][1]:.1f}"
        for i in range(len(points) - 1):
            p0 = points[i]
            p1 = points[i + 1]
            cx1 = p0[0] + (p1[0] - p0[0]) * 0.45
            cy1 = p0[1]
            cx2 = p0[0] + (p1[0] - p0[0]) * 0.55
            cy2 = p1[1]
            line_path += f" C {cx1:.1f} {cy1:.1f}, {cx2:.1f} {cy2:.1f}, {p1[0]:.1f} {p1[1]:.1f}"
        
        area_path = f"{line_path} L {points[-1][0]:.1f} {base_y} L {points[0][0]:.1f} {base_y} Z"
    else:
        line_path = ""
        area_path = ""

    # Grid lines (4 horizontal steps)
    grid_lines = []
    for step in range(4):
        ratio = step / 3.0
        gy = base_y - ratio * plot_h
        g_val = int(round(ratio * y_max))
        grid_lines.append(f"""
        <line x1="{plot_x}" y1="{gy:.1f}" x2="{plot_x + plot_w}" y2="{gy:.1f}" stroke="#24283b" stroke-width="1" stroke-dasharray="3,4" />
        <text x="{plot_x - 12}" y="{gy + 4:.1f}" text-anchor="end" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif" font-size="11" fill="#565f89">{g_val}</text>
        """)

    # Date labels along X-axis
    x_labels = []
    for i in range(0, days_count, 5):
        pt = points[i]
        label = pt[2].strftime("%b %d")
        x_labels.append(f"""
        <text x="{pt[0]:.1f}" y="{base_y + 20}" text-anchor="middle" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif" font-size="11" fill="#565f89">{label}</text>
        """)

    # Dots for days with commits
    dots = []
    for x, y, date_obj, val in points:
        if val > 0:
            color = "#7dcfff" if val >= 3 else "#7aa2f7"
            radius = 4.5 if val >= 3 else 3.5
            dots.append(f"""
            <circle cx="{x:.1f}" cy="{y:.1f}" r="{radius}" fill="{color}" stroke="#1a1b26" stroke-width="2">
              <title>{date_obj.strftime('%b %d, %Y')}: {val} commit{'s' if val != 1 else ''}</title>
            </circle>
            """)

    svg_content = f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" fill="none">
  <defs>
    <linearGradient id="bg-grad" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#16161e" />
      <stop offset="100%" stop-color="#1a1b26" />
    </linearGradient>
    <linearGradient id="line-grad" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0%" stop-color="#7aa2f7" />
      <stop offset="50%" stop-color="#bb9af7" />
      <stop offset="100%" stop-color="#7dcfff" />
    </linearGradient>
    <linearGradient id="area-grad" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#7aa2f7" stop-opacity="0.32" />
      <stop offset="100%" stop-color="#7aa2f7" stop-opacity="0.0" />
    </linearGradient>
    <filter id="glow" x="-20%" y="-20%" width="140%" height="140%">
      <feGaussianBlur stdDeviation="2.5" result="blur" />
      <feMerge>
        <feMergeNode in="blur" />
        <feMergeNode in="SourceGraphic" />
      </feMerge>
    </filter>
  </defs>

  <!-- Background Card -->
  <rect width="{width}" height="{height}" rx="14" fill="url(#bg-grad)" stroke="#292e42" stroke-width="1.2" />

  <!-- Header -->
  <g transform="translate(35, 36)">
    <!-- Git Branch Icon -->
    <path d="M 0 0 L 0 14 M 0 3 A 3 3 0 0 1 6 3 M 6 3 L 6 9 A 3 3 0 0 1 12 9" stroke="#7aa2f7" stroke-width="2" stroke-linecap="round" fill="none" />
    
    <text x="22" y="10" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif" font-size="16" font-weight="700" fill="#c0caf5">
      rustline
      <tspan fill="#565f89" font-weight="400" font-size="13"> / commit activity (last 30 days)</tspan>
    </text>

    <!-- Metrics Badges (Right-aligned) -->
    <g transform="translate({plot_w - 60}, -6)">
      <!-- Total Commits Chip -->
      <rect x="0" y="0" width="130" height="24" rx="12" fill="#24283b" stroke="#414868" stroke-width="1" />
      <circle cx="12" cy="12" r="4" fill="#9ece6a" />
      <text x="24" y="16" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif" font-size="11" font-weight="600" fill="#a9b1d6">
        {total_commits} repo commits
      </text>
    </g>
  </g>

  <!-- Grid lines -->
  {''.join(grid_lines)}

  <!-- Area Fill -->
  <path d="{area_path}" fill="url(#area-grad)" />

  <!-- Trend Line -->
  <path d="{line_path}" fill="none" stroke="url(#line-grad)" stroke-width="2.8" stroke-linecap="round" stroke-linejoin="round" filter="url(#glow)" />

  <!-- Commit Dots -->
  {''.join(dots)}

  <!-- X-Axis Labels -->
  {''.join(x_labels)}

  <!-- Footer Info -->
  <g transform="translate(35, {height - 18})" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif" font-size="11" fill="#565f89">
    <text>⚡ Auto-generated exclusively from rustline repository history</text>
    <text x="{width - 70}" text-anchor="end" fill="#565f89">branch: main</text>
  </g>
</svg>
"""

    os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        f.write(svg_content.strip() + "\n")
    print(f"Generated {output_path} successfully ({total_commits} commits).")


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "dist/activity-graph.svg"
    dates = get_commit_dates()
    generate_svg(dates, out)
