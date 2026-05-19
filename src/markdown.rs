use crate::stats::Stats;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

pub fn generate_markdown_report(
    stats: &Stats,
    all_svg: &str,
    year_svgs: &[(String, String)],
) -> String {
    let mut md = String::new();

    md.push_str("# Total\n\n");
    md.push_str(&format!(
        "{} FROM {}\n\n",
        stats.total.total_duration_hr, stats.total.from_date
    ));
    md.push_str(&format!(
        "{} seconds ({:.2} %), as {}\n\n",
        stats.total.total_seconds,
        stats.total.total_ratio,
        stats.total.total_seconds_hr
    ));
    md.push_str(&format!(
        "{} times ({:.2} % of days), {} seconds every time\n\n",
        stats.total.total_times,
        stats.total.total_day_ratio,
        stats.total.mean_usage
    ));

    md.push_str("# Past N Stats\n\n");
    for p in &stats.past_n {
        md.push_str(&format!(
            "- {}: {} seconds ({:.2} %) = {} times ({:.2} % of days) * {} seconds every time\n\n",
            p.name, p.seconds, p.ratio, p.times, p.day_ratio, p.mean_usage
        ));
    }

    md.push_str("# Interval\n\n");
    md.push_str(&format!(
        "- day {}!\n- Max: {}\n- Mean: {} s, {}\n\n",
        stats.interval.current_interval,
        stats.interval.max_interval,
        stats.interval.mean_interval,
        stats.interval.mean_interval_hr
    ));

    md.push_str("# Activity Graph\n\n");

    let all_b64 = BASE64.encode(all_svg);
    md.push_str("## all\n\n");
    md.push_str(&format!(
        "![all](data:image/svg+xml;base64,{})\n\n",
        all_b64
    ));

    for (year, svg) in year_svgs {
        let b64 = BASE64.encode(svg);
        md.push_str(&format!("## {}\n\n", year));
        md.push_str(&format!(
            "![{}](data:image/svg+xml;base64,{})\n\n",
            year, b64
        ));
    }

    md
}
