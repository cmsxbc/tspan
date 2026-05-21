use chrono::{Datelike, NaiveDate};

// Exact parameters from reference/calendarimg.sh/calendarimg.sh
const INNER: i32 = 15;
const BORDER: i32 = 5;
const PADDING: i32 = 4;
const STRIDE: i32 = INNER + BORDER * 2 + PADDING;
const MARGIN_LEFT: i32 = 40;
const MARGIN_TOP: i32 = 30;

#[derive(Clone, Copy)]
pub struct ColorScheme {
    pub name: &'static str,
    pub base: &'static str,
    pub empty: &'static str,
    pub low: &'static str,
    pub medium: &'static str,
    pub high: &'static str,
}

impl ColorScheme {
    pub fn color_for_seconds(&self, seconds: i64) -> &'static str {
        if seconds == 0 {
            self.empty
        } else if seconds < 1800 {
            self.low
        } else if seconds < 3600 {
            self.medium
        } else {
            self.high
        }
    }

    pub fn color_idx_for_seconds(&self, seconds: i64) -> i32 {
        if seconds == 0 { 0 }
        else if seconds < 1800 { 1 }
        else if seconds < 3600 { 2 }
        else { 3 }
    }
}

pub const SCHEME_HEATMAP: ColorScheme = ColorScheme {
    name: "热力图",
    base: "#1f2328",
    empty: "#f6f8fa",
    low: "#9be9a8",
    medium: "#f9d71c",
    high: "#e5534b",
};

pub const SCHEME_TRAFFIC: ColorScheme = ColorScheme {
    name: "交通灯",
    base: "#f6f8fa",
    empty: "#ebedf0",
    low: "#2da44e",
    medium: "#f9d71c",
    high: "#cf222e",
};

pub const SCHEME_OCEAN: ColorScheme = ColorScheme {
    name: "海洋",
    base: "#0d1117",
    empty: "#f6f8fa",
    low: "#a5d8ff",
    medium: "#74c0fc",
    high: "#339af0",
};

pub fn scheme_by_name(name: &str) -> ColorScheme {
    match name {
        "traffic" => SCHEME_TRAFFIC,
        "ocean" => SCHEME_OCEAN,
        _ => SCHEME_HEATMAP,
    }
}

const DIGIT_PATTERNS: [[u8; 5]; 10] = [
    [0b11111, 0b10001, 0b10001, 0b10001, 0b11111],
    [0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
    [0b11111, 0b00001, 0b11111, 0b10000, 0b11111],
    [0b11111, 0b00001, 0b11111, 0b00001, 0b11111],
    [0b10001, 0b10001, 0b11111, 0b00001, 0b00001],
    [0b11111, 0b10000, 0b11111, 0b00001, 0b11111],
    [0b11111, 0b10000, 0b11111, 0b10001, 0b11111],
    [0b11111, 0b00001, 0b00001, 0b00001, 0b00001],
    [0b11111, 0b10001, 0b11111, 0b10001, 0b11111],
    [0b11111, 0b10001, 0b11111, 0b00001, 0b11111],
];

fn draw_dot_matrix_digit(svg: &mut String, digit: i32, x: i32, y: i32, pixel: i32, color: &str) {
    let pattern = DIGIT_PATTERNS[digit as usize];
    for row in 0..5 {
        for col in 0..5 {
            if (pattern[row] >> (4 - col)) & 1 == 1 {
                svg.push_str(&format!(
                    r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                    x + col as i32 * pixel, y + row as i32 * pixel, pixel, pixel, color
                ));
            }
        }
    }
}

fn draw_dot_matrix_number(svg: &mut String, number: i32, x: i32, y: i32, pixel: i32, gap: i32, color: &str) {
    let s = number.to_string();
    for (i, ch) in s.chars().enumerate() {
        let dx = x + i as i32 * (5 * pixel + gap);
        let d = ch.to_digit(10).unwrap_or(0) as i32;
        draw_dot_matrix_digit(svg, d, dx, y, pixel, color);
    }
}

fn month_label(month: u32) -> &'static str {
    match month {
        1 => "Jan", 2 => "Feb", 3 => "Mar", 4 => "Apr",
        5 => "May", 6 => "Jun", 7 => "Jul", 8 => "Aug",
        9 => "Sep", 10 => "Oct", 11 => "Nov", 12 => "Dec",
        _ => "",
    }
}

pub fn generate_svg_calendar(
    data: &[(String, i64)],
    year: Option<i32>,
    scheme: ColorScheme,
) -> String {
    if data.is_empty() {
        return "<svg width=\"100\" height=\"50\" xmlns=\"http://www.w3.org/2000/svg\"><text x=\"10\" y=\"30\" font-size=\"12\" fill=\"#666\">No data</text></svg>".to_string();
    }

    let (start_date, end_date) = if let Some(y) = year {
        let s = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
        let e = if y == chrono::Local::now().year() {
            chrono::Local::now().naive_local().date()
        } else {
            NaiveDate::from_ymd_opt(y, 12, 31).unwrap()
        };
        (s, e)
    } else {
        let first = NaiveDate::parse_from_str(&data[0].0, "%Y-%m-%d").unwrap();
        let last = NaiveDate::parse_from_str(&data[data.len() - 1].0, "%Y-%m-%d").unwrap();
        let today = chrono::Local::now().naive_local().date();
        (first, today.max(last))
    };

    let mut day_map = std::collections::HashMap::new();
    for (day, secs) in data {
        day_map.insert(day.clone(), *secs);
    }

    let total_days = (end_date - start_date).num_days() + 1;
    let cols = if year.is_some() {
        let offset = start_date.weekday().num_days_from_monday() as i64;
        (total_days + offset + 6) / 7
    } else {
        let rows = ((total_days as f64).sqrt().ceil() as i64).max(7);
        (total_days + rows - 1) / rows
    };

    let rows = if year.is_some() { 7 } else {
        ((total_days as f64).sqrt().ceil() as i64).max(7)
    };

    let right_margin = if year.is_some() { 40 } else { 20 };
    let bottom_margin = if year.is_some() { 48 } else { 30 };
    let svg_width = MARGIN_LEFT + cols as i32 * STRIDE - PADDING + right_margin;
    let svg_height = MARGIN_TOP + rows as i32 * STRIDE - PADDING + bottom_margin;

    let mut svg = format!(
        r#"<svg width="{}" height="{}" xmlns="http://www.w3.org/2000/svg">"#,
        svg_width, svg_height
    );

    let title = match year {
        Some(y) => format!("{}", y),
        None => "All Time".to_string(),
    };
    let dark = "#333333";
    svg.push_str(&format!(
        r#"<text x="5" y="18" font-size="18" font-weight="bold" fill="{}">{}</text>"#,
        dark, title
    ));

    let grey = "#666666";
    if year.is_some() {
        let weekdays = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        for (i, wd) in weekdays.iter().enumerate() {
            if i % 2 == 0 {
                let y = MARGIN_TOP + i as i32 * STRIDE + INNER / 2 + BORDER + 4;
                svg.push_str(&format!(
                    r#"<text x="5" y="{}" font-size="12" fill="{}">{}</text>"#,
                    y, grey, wd
                ));
            }
        }
    }

    // Build grid: (row, col) -> (color_idx, day_str, seconds, date)
    let mut grid: Vec<Vec<Option<(i32, String, i64, NaiveDate)>>> = vec![vec![None; cols as usize]; rows as usize];
    let mut col_counts = vec![0i32; cols as usize];
    let mut row_counts = vec![0i32; rows as usize];
    for row in 0..rows {
        for col in 0..cols {
            let day_idx = if year.is_some() {
                let offset = start_date.weekday().num_days_from_monday() as i64;
                col * 7 + row as i64 - offset
            } else {
                col * rows + row as i64
            };
            if day_idx < 0 || day_idx >= total_days {
                continue;
            }
            let date = start_date + chrono::Duration::days(day_idx);
            if date > end_date {
                continue;
            }
            let day_str = date.format("%Y-%m-%d").to_string();
            let seconds = day_map.get(&day_str).copied().unwrap_or(0);
            let color_idx = scheme.color_idx_for_seconds(seconds);
            if seconds > 0 {
                col_counts[col as usize] += 1;
                row_counts[row as usize] += 1;
            }
            grid[row as usize][col as usize] = Some((color_idx, day_str, seconds, date));
        }
    }

    let base_color = scheme.base;
    let mut prev_month = 0u32;

    // Phase 1: inner cell + inner borders + corners
    for row in 0..rows {
        for col in 0..cols {
            let cell = &grid[row as usize][col as usize];
            if cell.is_none() { continue; }
            let (color_idx, day_str, seconds, date) = cell.as_ref().unwrap();
            let cell_color = scheme.color_for_seconds(*seconds);

            let base_x = MARGIN_LEFT + col as i32 * STRIDE;
            let base_y = MARGIN_TOP + row as i32 * STRIDE;

            if year.is_some() && date.day() == 1 && date.month() != prev_month {
                prev_month = date.month();
                svg.push_str(&format!(
                    r#"<text x="{}" y="{}" font-size="12" fill="{}">{}</text>"#,
                    base_x + BORDER, MARGIN_TOP - 4, grey, month_label(date.month())
                ));
            }

            // Neighbor checks
            let up_same = if row > 0 {
                if let Some((nidx, _, _, _)) = &grid[(row-1) as usize][col as usize] { *nidx == *color_idx } else { false }
            } else { false };
            let right_same = if col + 1 < cols {
                if let Some((nidx, _, _, _)) = &grid[row as usize][(col+1) as usize] { *nidx == *color_idx } else { false }
            } else { false };
            let down_same = if row + 1 < rows {
                if let Some((nidx, _, _, _)) = &grid[(row+1) as usize][col as usize] { *nidx == *color_idx } else { false }
            } else { false };
            let left_same = if col > 0 {
                if let Some((nidx, _, _, _)) = &grid[row as usize][(col-1) as usize] { *nidx == *color_idx } else { false }
            } else { false };

            // Inner cell
            let tooltip = format!("{}: {}s ({})", day_str, seconds, crate::stats::human_readable_time(*seconds));
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"><title>{}</title></rect>"#,
                base_x + BORDER, base_y + BORDER, INNER, INNER, cell_color, tooltip
            ));

            // Inner borders (EDGE part in reference)
            let top_color = if up_same { cell_color } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x + BORDER, base_y, INNER, BORDER, top_color
            ));
            let right_color = if right_same { cell_color } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x + BORDER + INNER, base_y + BORDER, BORDER, INNER, right_color
            ));
            let bottom_color = if down_same { cell_color } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x + BORDER, base_y + BORDER + INNER, INNER, BORDER, bottom_color
            ));
            let left_color = if left_same { cell_color } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x, base_y + BORDER, BORDER, INNER, left_color
            ));

            // Corners (draw_corner logic from reference)
            // corner 0 = top-left
            let c0 = if up_same && left_same {
                let ul_up = if col > 0 && row > 0 {
                    if let Some((nidx, _, _, _)) = &grid[(row-1) as usize][(col-1) as usize] { *nidx == *color_idx } else { false }
                } else { false };
                if ul_up { cell_color } else { base_color }
            } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x, base_y, BORDER, BORDER, c0
            ));

            // corner 1 = top-right
            let c1 = if up_same && right_same {
                let ur_up = if col + 1 < cols && row > 0 {
                    if let Some((nidx, _, _, _)) = &grid[(row-1) as usize][(col+1) as usize] { *nidx == *color_idx } else { false }
                } else { false };
                if ur_up { cell_color } else { base_color }
            } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x + BORDER + INNER, base_y, BORDER, BORDER, c1
            ));

            // corner 2 = bottom-right
            let c2 = if right_same && down_same {
                let br_down = if col + 1 < cols && row + 1 < rows {
                    if let Some((nidx, _, _, _)) = &grid[(row+1) as usize][(col+1) as usize] { *nidx == *color_idx } else { false }
                } else { false };
                if br_down { cell_color } else { base_color }
            } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x + BORDER + INNER, base_y + BORDER + INNER, BORDER, BORDER, c2
            ));

            // corner 3 = bottom-left
            let c3 = if down_same && left_same {
                let dl_down = if col > 0 && row + 1 < rows {
                    if let Some((nidx, _, _, _)) = &grid[(row+1) as usize][(col-1) as usize] { *nidx == *color_idx } else { false }
                } else { false };
                if dl_down { cell_color } else { base_color }
            } else { base_color };
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
                base_x, base_y + BORDER + INNER, BORDER, BORDER, c3
            ));
        }
    }

    // Weekly / daily-of-week counts (year view only) — seven-segment style
    if year.is_some() {
        let pixel = 3;
        let dig_gap = 2;
        let seg_color = scheme.base;
        let dig_w = 5 * pixel;
        let dig_h = 5 * pixel;
        for row in 0..rows {
            let cy = MARGIN_TOP + row as i32 * STRIDE + STRIDE / 2;
            let y = cy - dig_h / 2;
            let x = MARGIN_LEFT + cols as i32 * STRIDE + 2;
            draw_dot_matrix_number(&mut svg, row_counts[row as usize], x, y, pixel, dig_gap, seg_color);
        }
        for col in 0..cols {
            let cx = MARGIN_LEFT + col as i32 * STRIDE + BORDER + INNER / 2;
            let num_w = if col_counts[col as usize] >= 10 { dig_w * 2 + dig_gap } else { dig_w };
            let x = cx - num_w / 2;
            let y = MARGIN_TOP + rows as i32 * STRIDE + 6;
            draw_dot_matrix_number(&mut svg, col_counts[col as usize], x, y, pixel, dig_gap, seg_color);
        }
    }

    // Legend
    let legend_y = if year.is_some() {
        MARGIN_TOP + rows as i32 * STRIDE + 32
    } else {
        svg_height - 20
    };
    let legend_x = MARGIN_LEFT;
    let levels = [(0, "0s"), (1, "<30m"), (1800, "30-60m"), (3600, ">60m")];
    for (i, (secs, label)) in levels.iter().enumerate() {
        let x = legend_x + i as i32 * 60;
        let color = scheme.color_for_seconds(*secs);
        svg.push_str(&format!(
            r#"<rect x="{}" y="{}" width="10" height="10" rx="2" fill="{}"/><text x="{}" y="{}" font-size="11" fill="{}">{}</text>"#,
            x, legend_y, color, x + 14, legend_y + 9, grey, label
        ));
    }

    svg.push_str("</svg>");
    svg
}

pub fn generate_all_years_svgs(data: &[(String, i64)], scheme: ColorScheme) -> Vec<(String, String)> {
    let mut years_data: std::collections::HashMap<i32, Vec<(String, i64)>> = std::collections::HashMap::new();
    for (day, secs) in data {
        if let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") {
            years_data.entry(date.year()).or_default().push((day.clone(), *secs));
        }
    }

    let mut years: Vec<i32> = years_data.keys().copied().collect();
    years.sort_by(|a, b| b.cmp(a));

    years.into_iter()
        .map(|y| {
            let svg = generate_svg_calendar(&years_data[&y], Some(y), scheme);
            (y.to_string(), svg)
        })
        .collect()
}
