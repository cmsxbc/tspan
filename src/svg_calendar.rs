use chrono::{Datelike, NaiveDate};

const CELL_SIZE: i32 = 15;
const CELL_GAP: i32 = 4;
const MARGIN_LEFT: i32 = 40;
const MARGIN_TOP: i32 = 20;

fn color_for_seconds(seconds: i64) -> &'static str {
    if seconds == 0 {
        "#ebedf0"
    } else if seconds < 1800 {
        "#9be9a8"
    } else if seconds < 3600 {
        "#f9d71c"
    } else {
        "#e5534b"
    }
}

fn color_idx_for_seconds(seconds: i64) -> i32 {
    if seconds == 0 { 0 }
    else if seconds < 1800 { 1 }
    else if seconds < 3600 { 2 }
    else { 3 }
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

    // Build day -> seconds map
    let mut day_map = std::collections::HashMap::new();
    for (day, secs) in data {
        day_map.insert(day.clone(), *secs);
    }

    // Compute grid dimensions
    let total_days = (end_date - start_date).num_days() + 1;
    let cols = if year.is_some() {
        // For yearly view: always 7 rows, compute cols based on year days
        let offset = start_date.weekday().num_days_from_monday() as i64;
        (total_days + offset + 6) / 7
    } else {
        // For all-time view: approximate square
        let rows = ((total_days as f64).sqrt().ceil() as i64).max(7);
        (total_days + rows - 1) / rows
    };

    let rows = if year.is_some() { 7 } else {
        ((total_days as f64).sqrt().ceil() as i64).max(7)
    };

    let svg_width = MARGIN_LEFT + cols as i32 * (CELL_SIZE + CELL_GAP) + 20;
    let svg_height = MARGIN_TOP + rows as i32 * (CELL_SIZE + CELL_GAP) + 30;

    let mut svg = format!(
        r#"<svg width="{}" height="{}" xmlns="http://www.w3.org/2000/svg">"#,
        svg_width, svg_height
    );

    // Title
    let title = match year {
        Some(y) => format!("{}", y),
        None => "All Time".to_string(),
    };
    let dark = "#333333";
    svg.push_str(&format!(
        r#"<text x="{}" y="{}" font-size="14" font-weight="bold" fill="{}">{}</text>"#,
        MARGIN_LEFT, MARGIN_TOP - 5, dark, title
    ));

    // Weekday labels
    let weekdays = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let grey = "#666666";
    for (i, wd) in weekdays.iter().enumerate() {
        if i % 2 == 0 {
            let y = MARGIN_TOP + i as i32 * (CELL_SIZE + CELL_GAP) + CELL_SIZE / 2 + 4;
            svg.push_str(&format!(
                r#"<text x="5" y="{}" font-size="9" fill="{}">{}</text>"#,
                y, grey, wd
            ));
        }
    }

    // Build grid
    let gap_half = CELL_GAP / 2;
    let mut grid: Vec<Vec<Option<(i32, String, i64, NaiveDate)>>> = vec![vec![None; rows as usize]; cols as usize];
    for col in 0..cols {
        for row in 0..rows {
            let day_idx = if year.is_some() {
                let offset = start_date.weekday().num_days_from_monday() as i64;
                col * 7 + row as i64 - offset
            } else {
                col + row as i64 * cols
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
            let color_idx = color_idx_for_seconds(seconds);
            grid[col as usize][row as usize] = Some((color_idx, day_str, seconds, date));
        }
    }

    // Draw cells with connectivity
    let mut prev_month = 0u32;
    for col in 0..cols {
        for row in 0..rows {
            let cell = &grid[col as usize][row as usize];
            if cell.is_none() { continue; }
            let (color_idx, day_str, seconds, date) = cell.as_ref().unwrap();

            let base_x = MARGIN_LEFT + col as i32 * (CELL_SIZE + CELL_GAP);
            let base_y = MARGIN_TOP + row as i32 * (CELL_SIZE + CELL_GAP);

            let mut left_ext = 0;
            let mut up_ext = 0;
            let mut right_ext = 0;
            let mut down_ext = 0;

            if col > 0 {
                if let Some((nidx, _, _, _)) = &grid[(col-1) as usize][row as usize] {
                    if *nidx == *color_idx { left_ext = gap_half; }
                }
            }
            if row > 0 {
                if let Some((nidx, _, _, _)) = &grid[col as usize][(row-1) as usize] {
                    if *nidx == *color_idx { up_ext = gap_half; }
                }
            }
            if col + 1 < cols {
                if let Some((nidx, _, _, _)) = &grid[(col+1) as usize][row as usize] {
                    if *nidx == *color_idx { right_ext = gap_half; }
                }
            }
            if row + 1 < rows {
                if let Some((nidx, _, _, _)) = &grid[col as usize][(row+1) as usize] {
                    if *nidx == *color_idx { down_ext = gap_half; }
                }
            }

            // Month label on first occurrence
            if year.is_some() && date.day() == 1 && date.month() != prev_month {
                prev_month = date.month();
                svg.push_str(&format!(
                    r#"<text x="{}" y="{}" font-size="9" fill="{}">{}</text>"#,
                    base_x, MARGIN_TOP - 5, grey, month_label(date.month())
                ));
            }

            let x = base_x - left_ext;
            let y = base_y - up_ext;
            let w = CELL_SIZE + left_ext + right_ext;
            let h = CELL_SIZE + up_ext + down_ext;
            let color = color_for_seconds(*seconds);
            let tooltip = format!("{}: {}s ({})", day_str, seconds, crate::stats::human_readable_time(*seconds));
            svg.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{}" stroke="rgba(0,0,0,0.05)" stroke-width="1">
                    <title>{}</title>
                </rect>"#,
                x, y, w, h, color, tooltip
            ));
        }
    }

    // Legend
    let legend_y = svg_height - 20;
    let legend_x = MARGIN_LEFT;
    let levels = [(0, "0s"), (1, "<30m"), (1800, "30-60m"), (3600, ">60m")];
    for (i, (secs, label)) in levels.iter().enumerate() {
        let x = legend_x + i as i32 * 60;
        let color = color_for_seconds(*secs);
        svg.push_str(&format!(
            r#"<rect x="{}" y="{}" width="10" height="10" rx="2" fill="{}"/><text x="{}" y="{}" font-size="9" fill="{}">{}</text>"#,
            x, legend_y, color, x + 14, legend_y + 9, grey, label
        ));
    }

    svg.push_str("</svg>");
    svg
}

pub fn generate_all_years_svgs(data: &[(String, i64)]) -> Vec<(String, String)> {
    let mut years_data: std::collections::HashMap<i32, Vec<(String, i64)>> = std::collections::HashMap::new();
    for (day, secs) in data {
        if let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") {
            years_data.entry(date.year()).or_default().push((day.clone(), *secs));
        }
    }

    let mut years: Vec<i32> = years_data.keys().copied().collect();
    years.sort_by(|a, b| b.cmp(a)); // Descending

    years.into_iter()
        .map(|y| {
            let svg = generate_svg_calendar(&years_data[&y], Some(y));
            (y.to_string(), svg)
        })
        .collect()
}
