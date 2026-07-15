use crate::config::{FanConfig, FanCurveConfig, FanCurveTail};
use std::time::Instant;

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
    if !temp_c.is_finite() || temp_c >= config.lv3 {
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

pub fn duty_for_temperature(temp_c: f64, thresholds: FanConfig, curve: FanCurveConfig) -> u8 {
    if !curve.enabled {
        return level_for_temperature(temp_c, thresholds).duty_percent();
    }
    if !temp_c.is_finite() {
        return curve.max_duty;
    }

    let temperatures = [
        thresholds.lv0,
        thresholds.lv1,
        thresholds.lv2,
        thresholds.lv3,
    ];
    let duties = [
        f64::from(curve.duty0),
        f64::from(curve.duty1),
        f64::from(curve.duty2),
        f64::from(curve.duty3),
    ];

    let raw_duty = if temp_c < temperatures[0] {
        0.0
    } else if temp_c <= temperatures[1] {
        interpolate(
            temp_c,
            temperatures[0],
            temperatures[1],
            duties[0],
            duties[1],
        )
    } else if temp_c <= temperatures[2] {
        interpolate(
            temp_c,
            temperatures[1],
            temperatures[2],
            duties[1],
            duties[2],
        )
    } else if temp_c <= temperatures[3] {
        interpolate(
            temp_c,
            temperatures[2],
            temperatures[3],
            duties[2],
            duties[3],
        )
    } else {
        match curve.tail {
            FanCurveTail::Hold => duties[3],
            FanCurveTail::Extrapolate => interpolate(
                temp_c,
                temperatures[2],
                temperatures[3],
                duties[2],
                duties[3],
            ),
        }
    };

    raw_duty.clamp(0.0, f64::from(curve.max_duty)).round() as u8
}

fn interpolate(value: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    y0 + (value - x0) * (y1 - y0) / (x1 - x0)
}

#[derive(Debug, Default)]
pub struct DutyStabilizer {
    applied_duty: Option<u8>,
    last_update: Option<Instant>,
}

impl DutyStabilizer {
    pub fn update(&mut self, target_duty: u8, curve: FanCurveConfig, now: Instant) -> u8 {
        let target_duty = if curve.enabled {
            target_duty.min(curve.max_duty)
        } else {
            target_duty
        };

        let Some(applied_duty) = self.applied_duty else {
            return self.force(target_duty, curve, now);
        };

        if !curve.enabled || target_duty == 0 || target_duty >= applied_duty {
            return self.force(target_duty, curve, now);
        }

        if applied_duty - target_duty <= curve.hysteresis {
            self.last_update = Some(now);
            return applied_duty;
        }

        if curve.ramp_down == 0 {
            return self.force(target_duty, curve, now);
        }

        let elapsed = self
            .last_update
            .map(|last_update| now.saturating_duration_since(last_update))
            .unwrap_or_default();
        let allowed_drop = (elapsed.as_secs_f64() * f64::from(curve.ramp_down))
            .floor()
            .min(f64::from(u8::MAX)) as u8;

        if allowed_drop == 0 {
            return applied_duty;
        }

        let next_duty = applied_duty.saturating_sub(allowed_drop).max(target_duty);
        self.applied_duty = Some(next_duty);
        self.last_update = Some(now);
        next_duty
    }

    pub fn force(&mut self, duty: u8, curve: FanCurveConfig, now: Instant) -> u8 {
        let duty = if curve.enabled {
            duty.min(curve.max_duty)
        } else {
            duty
        };
        self.applied_duty = Some(duty);
        self.last_update = Some(now);
        duty
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanDecision {
    pub cpu_temp_c: f64,
    pub hottest_drive_temp_c: Option<f64>,
    pub cpu_level: FanLevel,
    pub drive_level: Option<FanLevel>,
    pub level: FanLevel,
    pub cpu_duty_percent: u8,
    pub drive_duty_percent: Option<u8>,
    pub duty_percent: u8,
    pub active_low_duty: f64,
}

impl FanDecision {
    pub fn cpu_only(temp_c: f64, config: FanConfig) -> Self {
        Self::cpu_only_with_curve(temp_c, config, FanCurveConfig::default())
    }

    pub fn cpu_only_with_curve(temp_c: f64, config: FanConfig, curve: FanCurveConfig) -> Self {
        Self::from_temperatures_with_curve(temp_c, config, None, config, curve)
    }

    pub fn from_temperatures(
        cpu_temp_c: f64,
        cpu_config: FanConfig,
        hottest_drive_temp_c: Option<f64>,
        drive_config: FanConfig,
    ) -> Self {
        Self::from_temperatures_with_curve(
            cpu_temp_c,
            cpu_config,
            hottest_drive_temp_c,
            drive_config,
            FanCurveConfig::default(),
        )
    }

    pub fn from_temperatures_with_curve(
        cpu_temp_c: f64,
        cpu_config: FanConfig,
        hottest_drive_temp_c: Option<f64>,
        drive_config: FanConfig,
        curve: FanCurveConfig,
    ) -> Self {
        let cpu_level = level_for_temperature(cpu_temp_c, cpu_config);
        let drive_level =
            hottest_drive_temp_c.map(|temp_c| level_for_temperature(temp_c, drive_config));
        let level = cpu_level.max(drive_level.unwrap_or(FanLevel::Off));
        let cpu_duty_percent = duty_for_temperature(cpu_temp_c, cpu_config, curve);
        let drive_duty_percent =
            hottest_drive_temp_c.map(|temp_c| duty_for_temperature(temp_c, drive_config, curve));
        let duty_percent = cpu_duty_percent.max(drive_duty_percent.unwrap_or(0));

        Self {
            cpu_temp_c,
            hottest_drive_temp_c,
            cpu_level,
            drive_level,
            level,
            cpu_duty_percent,
            drive_duty_percent,
            duty_percent,
            active_low_duty: 1.0 - f64::from(duty_percent) / 100.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const CONFIG: FanConfig = FanConfig {
        lv0: 55.0,
        lv1: 62.0,
        lv2: 70.0,
        lv3: 78.0,
    };

    const CURVE: FanCurveConfig = FanCurveConfig {
        enabled: true,
        duty0: 0,
        duty1: 25,
        duty2: 50,
        duty3: 75,
        tail: FanCurveTail::Extrapolate,
        max_duty: 100,
        hysteresis: 1,
        ramp_down: 0,
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
    fn disabled_curve_preserves_legacy_step_duties() {
        let curve = FanCurveConfig::default();

        assert_eq!(duty_for_temperature(54.9, CONFIG, curve), 0);
        assert_eq!(duty_for_temperature(55.0, CONFIG, curve), 25);
        assert_eq!(duty_for_temperature(62.0, CONFIG, curve), 50);
        assert_eq!(duty_for_temperature(70.0, CONFIG, curve), 75);
        assert_eq!(duty_for_temperature(78.0, CONFIG, curve), 100);
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

    #[test]
    fn interpolates_between_curve_inflection_points() {
        let thresholds = FanConfig {
            lv0: 45.0,
            lv1: 50.0,
            lv2: 55.0,
            lv3: 60.0,
        };

        let cases = [
            (44.9, 0),
            (45.0, 0),
            (47.0, 10),
            (47.5, 13),
            (50.0, 25),
            (52.0, 35),
            (52.5, 38),
            (55.0, 50),
            (57.0, 60),
            (57.5, 63),
            (60.0, 75),
        ];

        for (temperature, expected) in cases {
            assert_eq!(
                duty_for_temperature(temperature, thresholds, CURVE),
                expected,
                "temperature {temperature}C"
            );
        }
    }

    #[test]
    fn nonzero_first_inflection_still_turns_off_below_lv0() {
        let curve = FanCurveConfig {
            duty0: 20,
            duty1: 40,
            duty2: 60,
            duty3: 80,
            ..CURVE
        };

        assert_eq!(duty_for_temperature(CONFIG.lv0 - 0.1, CONFIG, curve), 0);
        assert_eq!(duty_for_temperature(CONFIG.lv0, CONFIG, curve), 20);
    }

    #[test]
    fn extrapolates_last_segment_until_hard_maximum() {
        let thresholds = FanConfig {
            lv0: 45.0,
            lv1: 50.0,
            lv2: 55.0,
            lv3: 60.0,
        };

        assert_eq!(duty_for_temperature(61.0, thresholds, CURVE), 80);
        assert_eq!(duty_for_temperature(62.5, thresholds, CURVE), 88);
        assert_eq!(duty_for_temperature(65.0, thresholds, CURVE), 100);
        assert_eq!(duty_for_temperature(100.0, thresholds, CURVE), 100);
    }

    #[test]
    fn hold_tail_keeps_last_inflection_duty() {
        let curve = FanCurveConfig {
            tail: FanCurveTail::Hold,
            ..CURVE
        };

        assert_eq!(duty_for_temperature(CONFIG.lv3, CONFIG, curve), 75);
        assert_eq!(duty_for_temperature(100.0, CONFIG, curve), 75);
    }

    #[test]
    fn maximum_caps_inflection_points_and_extrapolated_tail() {
        let curve = FanCurveConfig {
            duty0: 25,
            duty1: 50,
            duty2: 75,
            duty3: 100,
            max_duty: 80,
            ..CURVE
        };

        assert_eq!(duty_for_temperature(CONFIG.lv2, CONFIG, curve), 75);
        assert_eq!(duty_for_temperature(CONFIG.lv3, CONFIG, curve), 80);
        assert_eq!(duty_for_temperature(200.0, CONFIG, curve), 80);
    }

    #[test]
    fn flat_extrapolated_tail_stays_flat() {
        let curve = FanCurveConfig {
            duty2: 60,
            duty3: 60,
            ..CURVE
        };

        assert_eq!(duty_for_temperature(200.0, CONFIG, curve), 60);
    }

    #[test]
    fn non_finite_temperature_uses_configured_fail_safe_maximum() {
        assert_eq!(duty_for_temperature(f64::NAN, CONFIG, CURVE), 100);
        assert_eq!(duty_for_temperature(f64::INFINITY, CONFIG, CURVE), 100);
        assert_eq!(duty_for_temperature(f64::MAX, CONFIG, CURVE), 100);
    }

    #[test]
    fn curve_is_monotonic_and_never_exceeds_maximum() {
        let curve = FanCurveConfig {
            max_duty: 80,
            ..CURVE
        };
        let mut previous = 0;

        for step in 0..=1_000 {
            let temperature = 20.0 + f64::from(step) / 10.0;
            let duty = duty_for_temperature(temperature, CONFIG, curve);
            assert!(duty >= previous, "curve fell at {temperature}C");
            assert!(duty <= curve.max_duty);
            previous = duty;
        }
    }

    #[test]
    fn compares_cpu_and_drive_by_continuous_duty_not_level() {
        let cpu_thresholds = FanConfig {
            lv0: 40.0,
            lv1: 60.0,
            lv2: 70.0,
            lv3: 80.0,
        };
        let drive_thresholds = FanConfig {
            lv0: 45.0,
            lv1: 50.0,
            lv2: 55.0,
            lv3: 60.0,
        };
        let decision = FanDecision::from_temperatures_with_curve(
            50.0,
            cpu_thresholds,
            Some(49.0),
            drive_thresholds,
            CURVE,
        );

        assert_eq!(decision.cpu_level, FanLevel::Lv0);
        assert_eq!(decision.drive_level, Some(FanLevel::Lv0));
        assert_eq!(decision.cpu_duty_percent, 13);
        assert_eq!(decision.drive_duty_percent, Some(20));
        assert_eq!(decision.duty_percent, 20);
        assert!((decision.active_low_duty - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn stabilizer_applies_initial_and_upward_targets_immediately() {
        let curve = FanCurveConfig {
            enabled: true,
            max_duty: 80,
            hysteresis: 2,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(60, curve, started), 60);
        assert_eq!(
            stabilizer.update(100, curve, started + Duration::from_millis(10)),
            80
        );
    }

    #[test]
    fn stabilizer_holds_downward_changes_inside_deadband() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 2,
            ramp_down: 0,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(60, curve, started), 60);
        assert_eq!(
            stabilizer.update(58, curve, started + Duration::from_secs(1)),
            60
        );
        assert_eq!(
            stabilizer.update(57, curve, started + Duration::from_secs(2)),
            57
        );
    }

    #[test]
    fn stabilizer_reapplies_deadband_after_each_ramp_step() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 2,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(60, curve, started), 60);
        assert_eq!(
            stabilizer.update(54, curve, started + Duration::from_secs(1)),
            55
        );
        assert_eq!(
            stabilizer.update(54, curve, started + Duration::from_secs(2)),
            55
        );
    }

    #[test]
    fn stabilizer_limits_only_downward_rate_without_double_spending_time() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 0,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(80, curve, started), 80);
        assert_eq!(
            stabilizer.update(40, curve, started + Duration::from_secs(1)),
            75
        );
        assert_eq!(
            stabilizer.update(40, curve, started + Duration::from_secs(1)),
            75
        );
        assert_eq!(
            stabilizer.update(40, curve, started + Duration::from_secs(3)),
            65
        );
        assert_eq!(
            stabilizer.update(70, curve, started + Duration::from_secs(3)),
            70
        );
    }

    #[test]
    fn stabilizer_does_not_bank_ramp_time_while_target_is_unchanged() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 0,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(80, curve, started), 80);
        assert_eq!(
            stabilizer.update(80, curve, started + Duration::from_secs(60)),
            80
        );
        assert_eq!(
            stabilizer.update(40, curve, started + Duration::from_millis(60_100)),
            80
        );
        assert_eq!(
            stabilizer.update(40, curve, started + Duration::from_millis(60_250)),
            79
        );
    }

    #[test]
    fn stabilizer_never_ramps_below_target() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 0,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(80, curve, started), 80);
        assert_eq!(
            stabilizer.update(78, curve, started + Duration::from_secs(1)),
            78
        );
        assert_eq!(
            stabilizer.update(78, curve, started + Duration::from_secs(2)),
            78
        );
    }

    #[test]
    fn stabilizer_turns_off_and_reenables_immediately() {
        let curve = FanCurveConfig {
            enabled: true,
            hysteresis: 2,
            ramp_down: 5,
            ..CURVE
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(80, curve, started), 80);
        assert_eq!(
            stabilizer.update(0, curve, started + Duration::from_millis(10)),
            0
        );
        assert_eq!(
            stabilizer.update(55, curve, started + Duration::from_millis(20)),
            55
        );
        assert_eq!(
            stabilizer.force(0, curve, started + Duration::from_millis(30)),
            0
        );
    }

    #[test]
    fn disabled_curve_bypasses_stability_controls() {
        let curve = FanCurveConfig {
            hysteresis: 100,
            ramp_down: 1,
            ..FanCurveConfig::default()
        };
        let started = Instant::now();
        let mut stabilizer = DutyStabilizer::default();

        assert_eq!(stabilizer.update(75, curve, started), 75);
        assert_eq!(
            stabilizer.update(25, curve, started + Duration::from_millis(1)),
            25
        );
    }
}
