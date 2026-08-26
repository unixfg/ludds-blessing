use crate::error::{CoreError, Result};

const PLAYER_XP_PER_LEVEL: [u64; 15] = [
    0, 50_000, 70_000, 90_000, 100_000, 300_000, 500_000, 700_000, 900_000, 1_000_000, 1_200_000,
    1_400_000, 1_600_000, 1_800_000, 2_000_000,
];
const OFFICER_XP_PER_LEVEL: [u64; 10] = [
    0, 12_000, 20_000, 35_000, 45_000, 50_000, 50_000, 50_000, 50_000, 50_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rc8Progression {
    pub player_max_level: u32,
    pub skill_points_per_level: u32,
    pub story_points_per_level: u32,
    pub bonus_xp_use_mult_at_max_level: u32,
    pub officer_xp_required_mult: u32,
    pub officer_max_level: u32,
}

impl Default for Rc8Progression {
    fn default() -> Self {
        Self {
            player_max_level: 15,
            skill_points_per_level: 1,
            story_points_per_level: 4,
            bonus_xp_use_mult_at_max_level: 3,
            officer_xp_required_mult: 4,
            officer_max_level: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerProgress {
    pub story_checkpoint_xp: u64,
    pub xp: u64,
    pub bonus_xp: u64,
    pub deferred_bonus_xp: u64,
    pub level: u32,
    pub skill_points: u32,
    pub story_points: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficerProgress {
    pub xp: u64,
    pub bonus_xp: u64,
    pub level: u32,
    pub skill_points: u32,
}

pub fn player_xp_for_level(level: u32) -> u64 {
    if level <= 1 {
        return 0;
    }
    let configured_max = Rc8Progression::default().player_max_level;
    let specified = PLAYER_XP_PER_LEVEL.len() as u32;
    let mut total: u64 = PLAYER_XP_PER_LEVEL.iter().sum();
    if level <= specified {
        return PLAYER_XP_PER_LEVEL[..level as usize].iter().sum();
    }
    let mut last = *PLAYER_XP_PER_LEVEL.last().unwrap();
    for _ in specified..level.min(configured_max) {
        last = ((last as f32) * 1.1_f32) as u64;
        total = total.saturating_add(last);
    }
    if level > configured_max {
        total = total.saturating_add((PLAYER_XP_PER_LEVEL[14] as f32 * 2.0_f32) as u64);
    }
    total
}

pub fn officer_xp_for_level(level: u32) -> u64 {
    if level <= 1 {
        return 0;
    }
    let specified = OFFICER_XP_PER_LEVEL.len() as u32;
    let base = if level <= specified {
        OFFICER_XP_PER_LEVEL[..level as usize].iter().sum::<u64>()
    } else {
        let mut total = OFFICER_XP_PER_LEVEL.iter().sum::<u64>();
        let mut last = *OFFICER_XP_PER_LEVEL.last().unwrap();
        for _ in specified..level {
            last = ((last as f32) * 1.1_f32) as u64;
            total = total.saturating_add(last);
        }
        total
    };
    ((base as f32) * Rc8Progression::default().officer_xp_required_mult as f32) as u64
}

pub fn grant_player_xp(current: &PlayerProgress, source_xp: u64) -> Result<PlayerProgress> {
    validate_player_progress(current)?;
    validate_java_long(source_xp, "XP grant")?;
    let config = Rc8Progression::default();
    let mut next = current.clone();

    let bonus_multiplier = if current.level == config.player_max_level {
        config.bonus_xp_use_mult_at_max_level as f32
    } else {
        1.0_f32
    };
    // The game performs Java l2f -> fmul -> f2l, including the loss of integer
    // precision above 2^24. Keeping this cast order is compatibility-critical.
    let maximum_bonus_used = ((source_xp as f32) * bonus_multiplier) as u64;
    let bonus_used = next.bonus_xp.min(maximum_bonus_used);
    next.bonus_xp -= bonus_used;
    next.xp = next
        .xp
        .checked_add(source_xp)
        .and_then(|value| value.checked_add(bonus_used))
        .ok_or_else(|| CoreError::invalid_edit("XP grant overflows the save format"))?;
    validate_java_long(next.xp, "player XP")?;

    award_story_points(&mut next, config)?;
    let old_level = next.level;
    next.level = player_level_for_xp(next.xp, config.player_max_level).max(old_level);
    if next.level > old_level {
        let gained = next.level - old_level;
        next.skill_points = next
            .skill_points
            .checked_add(gained.saturating_mul(config.skill_points_per_level))
            .ok_or_else(|| CoreError::invalid_edit("skill-point total overflow"))?;
    }
    if old_level < config.player_max_level && next.level >= config.player_max_level {
        next.bonus_xp = next
            .bonus_xp
            .checked_add(next.deferred_bonus_xp)
            .ok_or_else(|| CoreError::invalid_edit("bonus-XP total overflow"))?;
        validate_java_long(next.bonus_xp, "bonus XP")?;
        // RC8 intentionally retains db after making the same amount available
        // in bx. Do not clear it.
    }

    if next.level >= config.player_max_level {
        let base = player_xp_for_level(config.player_max_level);
        let threshold = player_xp_for_level(config.player_max_level + 1);
        let cycle = fallback_story_step(threshold - base);
        if next.xp >= threshold {
            let cycles = (next.xp - threshold) / cycle + 1;
            let reduction = cycles
                .checked_mul(cycle)
                .ok_or_else(|| CoreError::invalid_edit("max-level XP wrap overflow"))?;
            next.xp = next
                .xp
                .checked_sub(reduction)
                .ok_or_else(|| CoreError::validation("invalid max-level XP wrap"))?;
            next.story_checkpoint_xp = next
                .story_checkpoint_xp
                .checked_sub(reduction)
                .ok_or_else(|| CoreError::validation("invalid max-level story checkpoint wrap"))?;
        }
    }
    Ok(next)
}

pub fn player_source_xp_to_reach(current: &PlayerProgress, target_level: u32) -> Result<u64> {
    let config = Rc8Progression::default();
    if target_level < current.level {
        return Err(CoreError::invalid_edit("player level cannot be reduced"));
    }
    if target_level > config.player_max_level {
        return Err(CoreError::invalid_edit(format!(
            "RC8 player level cannot exceed {}",
            config.player_max_level
        )));
    }
    let target_xp = player_xp_for_level(target_level);
    if current.xp >= target_xp {
        return Ok(0);
    }
    let mut low = 0u64;
    let mut high = target_xp - current.xp;
    while low < high {
        let middle = low + (high - low) / 2;
        if grant_player_xp(current, middle)?.xp >= target_xp {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    Ok(low)
}

pub fn raise_player_to_level(
    current: &PlayerProgress,
    target_level: u32,
) -> Result<PlayerProgress> {
    let source = player_source_xp_to_reach(current, target_level)?;
    grant_player_xp(current, source)
}

pub fn grant_officer_xp(
    current: &OfficerProgress,
    source_xp: u64,
    max_level: u32,
) -> Result<OfficerProgress> {
    if current.level == 0 {
        return Err(CoreError::validation("officer level must be at least one"));
    }
    if max_level < current.level {
        return Err(CoreError::invalid_edit(
            "officer maximum is below current level",
        ));
    }
    validate_java_long(source_xp, "officer XP grant")?;
    if current.level >= max_level {
        if source_xp == 0 {
            return Ok(current.clone());
        }
        return Err(CoreError::invalid_edit(
            "officer is already at the supported maximum level",
        ));
    }
    let mut next = current.clone();
    let maximum_bonus_used = source_xp as f32 as u64;
    let bonus_used = next.bonus_xp.min(maximum_bonus_used);
    next.bonus_xp -= bonus_used;
    next.xp = next
        .xp
        .checked_add(source_xp)
        .and_then(|value| value.checked_add(bonus_used))
        .ok_or_else(|| CoreError::invalid_edit("officer XP grant overflows"))?;
    validate_java_long(next.xp, "officer XP")?;
    Ok(next)
}

pub fn raise_officer_to_level(
    current: &OfficerProgress,
    target_level: u32,
    max_level: u32,
) -> Result<OfficerProgress> {
    if target_level < current.level {
        return Err(CoreError::invalid_edit("officer level cannot be reduced"));
    }
    if target_level > max_level {
        return Err(CoreError::invalid_edit(
            "target exceeds this officer's level cap",
        ));
    }
    let mut next = current.clone();
    next.level = target_level;
    next.xp = next.xp.max(officer_xp_for_level(target_level));
    validate_java_long(next.xp, "officer XP")?;
    Ok(next)
}

fn validate_player_progress(progress: &PlayerProgress) -> Result<()> {
    if progress.level == 0 || progress.level > Rc8Progression::default().player_max_level {
        return Err(CoreError::validation("invalid RC8 player level"));
    }
    if progress.story_checkpoint_xp > progress.xp {
        return Err(CoreError::validation(
            "story checkpoint is ahead of player XP",
        ));
    }
    for (value, field) in [
        (progress.story_checkpoint_xp, "story checkpoint"),
        (progress.xp, "player XP"),
        (progress.bonus_xp, "bonus XP"),
        (progress.deferred_bonus_xp, "deferred bonus XP"),
    ] {
        validate_java_long(value, field)?;
    }
    Ok(())
}

fn player_level_for_xp(xp: u64, max_level: u32) -> u32 {
    let mut level = 1;
    for candidate in 2..=max_level {
        if xp >= player_xp_for_level(candidate) {
            level = candidate;
        } else {
            break;
        }
    }
    level
}

fn award_story_points(progress: &mut PlayerProgress, config: Rc8Progression) -> Result<()> {
    let max_base = player_xp_for_level(config.player_max_level);
    let max_step = fallback_story_step(
        (player_xp_for_level(config.player_max_level + 1) - max_base)
            / config.story_points_per_level as u64,
    );
    let mut gained = 0u64;
    loop {
        let threshold =
            next_story_checkpoint(progress.level, progress.story_checkpoint_xp, config)?;
        if threshold > progress.xp {
            break;
        }
        if threshold >= max_base {
            let count = (progress.xp - threshold) / max_step + 1;
            gained = gained
                .checked_add(count)
                .ok_or_else(|| CoreError::invalid_edit("story-point gain overflow"))?;
            progress.story_checkpoint_xp = threshold
                .checked_add((count - 1).saturating_mul(max_step))
                .ok_or_else(|| CoreError::invalid_edit("story checkpoint overflow"))?;
            break;
        }
        progress.story_checkpoint_xp = threshold;
        gained += 1;
    }
    progress.story_points = progress
        .story_points
        .checked_add(
            u32::try_from(gained)
                .map_err(|_| CoreError::invalid_edit("story-point gain exceeds supported range"))?,
        )
        .ok_or_else(|| CoreError::invalid_edit("story-point total overflow"))?;
    Ok(())
}

fn next_story_checkpoint(level: u32, last: u64, config: Rc8Progression) -> Result<u64> {
    let mut threshold = player_xp_for_level(level);
    let mut next_level_xp = player_xp_for_level(level + 1);
    let mut step =
        fallback_story_step((next_level_xp - threshold) / config.story_points_per_level as u64);
    let mut level_cursor = level + 1;
    let mut index = 1u32;
    loop {
        threshold = threshold
            .checked_add(step)
            .ok_or_else(|| CoreError::invalid_edit("story checkpoint overflow"))?;
        if threshold > last {
            return Ok(threshold);
        }
        if index.is_multiple_of(config.story_points_per_level)
            && level_cursor < config.player_max_level + 1
        {
            level_cursor += 1;
            let previous_next = next_level_xp;
            next_level_xp = player_xp_for_level(level_cursor);
            step = fallback_story_step(
                (next_level_xp - previous_next) / config.story_points_per_level as u64,
            );
        }
        index = index
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid_edit("story checkpoint iteration overflow"))?;
        if index > 128 && threshold < player_xp_for_level(config.player_max_level) {
            return Err(CoreError::validation("invalid story checkpoint state"));
        }
    }
}

const fn fallback_story_step(step: u64) -> u64 {
    if step == 0 {
        100_000
    } else {
        step
    }
}

fn validate_java_long(value: u64, field: &str) -> Result<()> {
    if value > i64::MAX as u64 {
        return Err(CoreError::invalid_edit(format!(
            "{field} exceeds the Java long range"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> PlayerProgress {
        PlayerProgress {
            story_checkpoint_xp: 0,
            xp: 0,
            bonus_xp: 0,
            deferred_bonus_xp: 0,
            level: 1,
            skill_points: 0,
            story_points: 0,
        }
    }

    #[test]
    fn rc8_player_thresholds_match_shipped_plugin() {
        assert_eq!(player_xp_for_level(1), 0);
        assert_eq!(player_xp_for_level(2), 50_000);
        assert_eq!(player_xp_for_level(6), 610_000);
        assert_eq!(player_xp_for_level(15), 11_710_000);
        assert_eq!(player_xp_for_level(16), 15_710_000);
    }

    #[test]
    fn grants_story_points_at_quarters_and_skill_points_at_levels() {
        let first_quarter = grant_player_xp(&fresh(), 12_500).unwrap();
        assert_eq!(first_quarter.story_points, 1);
        assert_eq!(first_quarter.level, 1);
        let level_two = grant_player_xp(&first_quarter, 37_500).unwrap();
        assert_eq!(level_two.story_points, 4);
        assert_eq!(level_two.skill_points, 1);
        assert_eq!(level_two.level, 2);
    }

    #[test]
    fn bonus_xp_and_deferred_release_are_accounted_for() {
        let mut state = fresh();
        state.bonus_xp = 10_000;
        let result = grant_player_xp(&state, 7_500).unwrap();
        assert_eq!(result.xp, 15_000);
        assert_eq!(result.bonus_xp, 2_500);

        let max_xp = player_xp_for_level(15);
        let at_fourteen = PlayerProgress {
            story_checkpoint_xp: player_xp_for_level(14),
            xp: max_xp - 1,
            deferred_bonus_xp: 123,
            level: 14,
            ..fresh()
        };
        let maxed = grant_player_xp(&at_fourteen, 1).unwrap();
        assert_eq!(maxed.level, 15);
        assert_eq!(maxed.deferred_bonus_xp, 123);
        assert_eq!(maxed.bonus_xp, 123);
    }

    #[test]
    fn raise_uses_minimum_source_xp_when_bonus_is_present() {
        let mut state = fresh();
        state.bonus_xp = 50_000;
        let source = player_source_xp_to_reach(&state, 2).unwrap();
        assert_eq!(source, 25_000);
        assert_eq!(raise_player_to_level(&state, 2).unwrap().xp, 50_000);
    }

    #[test]
    fn officer_thresholds_and_points_match_rc8() {
        assert_eq!(officer_xp_for_level(2), 48_000);
        assert_eq!(officer_xp_for_level(4), 268_000);
        let officer = OfficerProgress {
            xp: 47_999,
            bonus_xp: 0,
            level: 1,
            skill_points: 0,
        };
        let result = grant_officer_xp(&officer, 1, 5).unwrap();
        assert_eq!(result.xp, 48_000);
        assert_eq!(result.level, 1);
        assert_eq!(result.skill_points, 0);
    }

    #[test]
    fn java_float_rounding_is_preserved_for_bonus_xp() {
        let mut state = fresh();
        state.bonus_xp = 100_000_000;
        let result = grant_player_xp(&state, 16_777_217).unwrap();
        assert_eq!(result.bonus_xp, 83_222_784);

        let maxed = PlayerProgress {
            story_checkpoint_xp: player_xp_for_level(15),
            xp: player_xp_for_level(15),
            bonus_xp: 100_000_000,
            deferred_bonus_xp: 0,
            level: 15,
            skill_points: 0,
            story_points: 0,
        };
        let result = grant_player_xp(&maxed, 16_777_217).unwrap();
        assert_eq!(maxed.bonus_xp - result.bonus_xp, 50_331_648);
    }

    #[test]
    fn max_level_story_cycles_wrap_xp_and_checkpoint() {
        let max = player_xp_for_level(15);
        let state = PlayerProgress {
            story_checkpoint_xp: max,
            xp: max,
            bonus_xp: 0,
            deferred_bonus_xp: 0,
            level: 15,
            skill_points: 0,
            story_points: 0,
        };
        let once = grant_player_xp(&state, 4_000_000).unwrap();
        assert_eq!(once.xp, max);
        assert_eq!(once.story_checkpoint_xp, max);
        assert_eq!(once.story_points, 4);
        let twice = grant_player_xp(&state, 8_000_000).unwrap();
        assert_eq!(twice.xp, max);
        assert_eq!(twice.story_points, 8);
    }
}
