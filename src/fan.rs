use crate::config::FanConfig;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum FanLevel {
    Off,
    Lv0,
    Lv1,
    Lv2,
    Lv3,
}

impl FanLevel {
    pub fn duty_percent(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Lv0 => 25,
            Self::Lv1 => 50,
            Self::Lv2 => 75,
            Self::Lv3 => 100,
        }
    }

    pub fn active_low_duty(self) -> f64 {
        1.0 - (f64::from(self.duty_percent()) / 100.0)
    }
}

pub fn level_for_temperature(temp_c: f64, config: FanConfig) -> FanLevel {
    if temp_c >= config.lv3 {
        FanLevel::Lv3
    } else if temp_c >= config.lv2 {
        FanLevel::Lv2
    } else if temp_c >= config.lv1 {
        FanLevel::Lv1
    } else if temp_c >= config.lv0 {
        FanLevel::Lv0
    } else {
        FanLevel::Off
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanDecision {
    pub cpu_temp_c: f64,
    pub hottest_drive_temp_c: Option<f64>,
    pub cpu_level: FanLevel,
    pub drive_level: Option<FanLevel>,
    pub level: FanLevel,
    pub duty_percent: u8,
    pub active_low_duty: f64,
}

impl FanDecision {
    pub fn cpu_only(temp_c: f64, config: FanConfig) -> Self {
        Self::from_temperatures(temp_c, config, None, config)
    }

    pub fn from_temperatures(
        cpu_temp_c: f64,
        cpu_config: FanConfig,
        hottest_drive_temp_c: Option<f64>,
        drive_config: FanConfig,
    ) -> Self {
        let cpu_level = level_for_temperature(cpu_temp_c, cpu_config);
        let drive_level =
            hottest_drive_temp_c.map(|temp_c| level_for_temperature(temp_c, drive_config));
        let level = cpu_level.max(drive_level.unwrap_or(FanLevel::Off));

        Self {
            cpu_temp_c,
            hottest_drive_temp_c,
            cpu_level,
            drive_level,
            level,
            duty_percent: level.duty_percent(),
            active_low_duty: level.active_low_duty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: FanConfig = FanConfig {
        lv0: 55.0,
        lv1: 62.0,
        lv2: 70.0,
        lv3: 78.0,
    };

    #[test]
    fn maps_temperatures_to_step_levels() {
        assert_eq!(level_for_temperature(54.9, CONFIG), FanLevel::Off);
        assert_eq!(level_for_temperature(55.0, CONFIG), FanLevel::Lv0);
        assert_eq!(level_for_temperature(61.9, CONFIG), FanLevel::Lv0);
        assert_eq!(level_for_temperature(62.0, CONFIG), FanLevel::Lv1);
        assert_eq!(level_for_temperature(69.9, CONFIG), FanLevel::Lv1);
        assert_eq!(level_for_temperature(70.0, CONFIG), FanLevel::Lv2);
        assert_eq!(level_for_temperature(77.9, CONFIG), FanLevel::Lv2);
        assert_eq!(level_for_temperature(78.0, CONFIG), FanLevel::Lv3);
    }

    #[test]
    fn preserves_upstream_active_low_duty_values() {
        assert_eq!(FanLevel::Off.active_low_duty(), 1.0);
        assert_eq!(FanLevel::Lv0.active_low_duty(), 0.75);
        assert_eq!(FanLevel::Lv1.active_low_duty(), 0.5);
        assert_eq!(FanLevel::Lv2.active_low_duty(), 0.25);
        assert_eq!(FanLevel::Lv3.active_low_duty(), 0.0);
    }

    #[test]
    fn drive_level_can_raise_final_fan_level() {
        let drive_config = FanConfig {
            lv0: 45.0,
            lv1: 50.0,
            lv2: 55.0,
            lv3: 60.0,
        };
        let decision = FanDecision::from_temperatures(50.0, CONFIG, Some(55.0), drive_config);

        assert_eq!(decision.cpu_level, FanLevel::Off);
        assert_eq!(decision.drive_level, Some(FanLevel::Lv2));
        assert_eq!(decision.level, FanLevel::Lv2);
        assert_eq!(decision.duty_percent, 75);
    }

    #[test]
    fn cpu_level_wins_when_it_is_higher() {
        let drive_config = FanConfig {
            lv0: 45.0,
            lv1: 50.0,
            lv2: 55.0,
            lv3: 60.0,
        };
        let decision = FanDecision::from_temperatures(70.0, CONFIG, Some(46.0), drive_config);

        assert_eq!(decision.cpu_level, FanLevel::Lv2);
        assert_eq!(decision.drive_level, Some(FanLevel::Lv0));
        assert_eq!(decision.level, FanLevel::Lv2);
    }

    #[test]
    fn missing_drive_temperature_preserves_cpu_only_behavior() {
        let decision = FanDecision::from_temperatures(62.0, CONFIG, None, CONFIG);

        assert_eq!(decision.cpu_level, FanLevel::Lv1);
        assert_eq!(decision.drive_level, None);
        assert_eq!(decision.level, FanLevel::Lv1);
        assert_eq!(decision.duty_percent, 50);
    }
}
