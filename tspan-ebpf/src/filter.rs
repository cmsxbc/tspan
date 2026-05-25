use regex::Regex;

#[derive(Debug, Clone)]
pub struct Filter {
    allow_uids: Vec<u32>,
    deny_regex: Option<Regex>,
}

impl Filter {
    pub fn new(allow_uids: Vec<u32>, deny_pattern: Option<String>) -> anyhow::Result<Self> {
        let deny_regex = match deny_pattern {
            Some(p) if !p.is_empty() => Some(Regex::new(&p)?),
            _ => None,
        };
        Ok(Self {
            allow_uids,
            deny_regex,
        })
    }

    pub fn allow(&self, uid: u32, command: &str) -> bool {
        if !self.allow_uids.is_empty() && !self.allow_uids.contains(&uid) {
            return false;
        }
        if let Some(re) = &self.deny_regex {
            if re.is_match(command) {
                return false;
            }
        }
        true
    }
}
