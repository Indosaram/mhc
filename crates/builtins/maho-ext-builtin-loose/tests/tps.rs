use maho_ext_builtin_loose::tps::Timing;
#[test]
fn only_assistant_intervals_count_and_turn_resets(){let mut timing=Timing::default();timing.start_message(100.0);timing.finish_message(1100.0);timing.start_message(9000.0);let stats=timing.finish_turn(10000.0,true,100.0,40.0,300.0,100.0).expect("stats");assert_eq!(stats.tokens_per_second,20.0);assert_eq!(stats.cache_hit_rate,60.0);assert_eq!(stats.elapsed_seconds,2.0);assert!(timing.finish_turn(11000.0,true,0.0,10.0,0.0,0.0).is_none());}
#[test]
fn overlapping_starts_finish_previous_interval(){let mut timing=Timing::default();timing.start_message(0.0);timing.start_message(500.0);assert_eq!(timing.finish_turn(1000.0,true,0.0,10.0,0.0,0.0).expect("stats").tokens_per_second,10.0);}
#[test]
fn headless_and_nonpositive_intervals_suppress_notice(){let mut timing=Timing::default();timing.start_message(100.0);assert!(timing.finish_turn(50.0,true,0.0,10.0,0.0,0.0).is_none());timing.start_message(100.0);assert!(timing.finish_turn(200.0,false,0.0,10.0,0.0,0.0).is_none());}
