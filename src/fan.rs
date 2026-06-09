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
    pub temp_c: f64,
    pub level: FanLevel,
    pub duty_percent: u8,
    pub active_low_duty: f64,
}

impl FanDecision {
    pub fn cpu_only(temp_c: f64, config: FanConfig) -> Self {
        let level = level_for_temperature(temp_c, config);
        Self {
            temp_c,
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
}
