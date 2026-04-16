netanvil-rate-control(7)

# NAME

netanvil-rate-control - rate control modes for netanvil-cli

# DESCRIPTION

*netanvil-cli* supports five rate control modes, selected via *--rate-mode*.
Each mode decides, at every coordinator tick (typically 100ms), how many
requests-per-second the I/O workers should target for the next interval. This
page documents the conceptual model and all rate-related flags; see
*netanvil-cli-test*(1) and *netanvil-cli-leader*(1) for the per-command
invocation.

# MODES

*static* (default)
	Hold a constant target rate for the duration of the test. The rate is
	set by *--rps* and never changes. Use for steady-state throughput
	measurement.

*step*
	Transition through a series of rate/time waypoints defined by *--steps*.
	Each waypoint takes effect at its offset from test start. Use for
	staircase load profiles.

*pid*
	Proportional-integral-derivative control. Drives a metric
	(latency, error rate, or external signal) toward a target value by
	adjusting RPS in log-rate space. Supports *autotuning* when gains are
	not manually specified. Use for "hold p99 at 200ms" style tests.

*ramp*
	Legacy baseline-and-multiplier mode. Learns a baseline latency during a
	warmup phase, then ramps RPS upward as long as observed latency stays
	below _multiplier_ x _baseline_. Backs off on error bursts. Superseded
	by *adaptive* for most use cases.

*adaptive*
	Constraint arbitration. Composes multiple constraints (latency ceiling,
	error rate cap, PID setpoint, external signals) and picks the RPS that
	satisfies all of them. See *CONSTRAINT ARBITRATION* below.

# COMMON FLAGS

*--rps* _N_
	Target requests per second. For *static*, this is the held rate. For
	*pid*/*ramp*/*adaptive*, this is the initial rate. Default: _100_.

*--rate-mode* _MODE_
	Rate control mode. One of _static_, _step_, _pid_, _ramp_, _adaptive_.
	Default: _static_.

*--scheduler* _DISCIPLINE_
	Arrival-time scheduling discipline. _constant_ spaces requests at fixed
	intervals (lowest jitter). _poisson_ samples exponential interarrivals
	(realistic bursty load). Default: _constant_.

*--rate-config* _FILE_
	Load a complete rate configuration from a JSON file. The file should
	contain a serialized *RateConfig::Adaptive* object. When specified,
	overrides all other rate-mode flags. Use for complex constraint
	arrangements that are tedious to express on the command line.

# STEP MODE

*--steps* _SPEC_
	Step waypoints in the format _offset1:rps1,offset2:rps2,..._ where
	each _offset_ is relative to test start. For example, the value
	_0s:100,5s:500,10s:200_ starts at 100 RPS, jumps to 500 RPS at t=5s,
	and drops to 200 RPS at t=10s. Offsets accept duration syntax
	(_s_, _ms_, _m_).

# PID MODE

*--pid-metric* _METRIC_
	Metric to track. One of _latency-p50_, _latency-p90_, _latency-p99_,
	_error-rate_, or _external:HEADER_ (see *--response-signal*).
	Default: _latency-p99_.

*--pid-target* _VALUE_
	Target value in the metric's natural units. Latency values are
	milliseconds; error rate is a percentage. Default: _200.0_.

*--pid-kp* _K_, *--pid-ki* _K_, *--pid-kd* _K_
	Proportional, integral, and derivative gains. Omit all three to trigger
	autotuning during startup (see *--pid-autotune-duration*).

*--pid-min-rps* _N_, *--pid-max-rps* _N_
	Hard floor and ceiling on the controller's output rate. Defaults:
	_10_ and _10000_.

*--pid-autotune-duration* _DURATION_
	Cohen-Coon step-response exploration window used when gains are not
	manually specified. The controller records the system response to a
	step input and computes gains from the identified plant parameters.
	Default: _3s_.

*--pid-constraint* _EXPR_ (repeatable)
	Additional constraint for composite PID. Format: _metric OP value_.
	Example values: _'latency-p99 < 500'_, _'error-rate < 2'_,
	_'external:load < 80'_. When constraints are present, the arbiter
	picks the max RPS at which *all* constraints are satisfied.

# RAMP MODE

*--ramp-warmup* _DURATION_
	Duration of the warmup phase during which the controller learns the
	baseline latency. Default: _10s_.

*--ramp-multiplier* _FACTOR_
	Acceptable latency multiplier over the baseline. A value of _3.0_ means
	"p99 can be 3x the warmup baseline before backing off." Default: _3.0_.

*--ramp-max-errors* _PERCENT_
	Maximum error rate (%) before forcing rate reduction. Default: _5.0_.

*--baseline-floor* _MS_
	Floor applied to the warmup baseline before multiplying. Prevents sub-
	millisecond services from getting thresholds below the OS jitter
	envelope. Default: _4.0_ (suits 100Hz-tick kernels).

# ADAPTIVE MODE

Adaptive mode composes multiple *Constraint* objects through an *Arbiter* that
applies a composition strategy (MinSelector: the lowest desired rate wins) and
a series of policy layers (progressive ceiling, known-good floor, cooldown
after backoff, per-tick rate-change limits). The result is a target RPS that
respects every configured constraint.

## Shortcut flags

For common single-constraint setups, shortcut flags avoid the verbosity of
*--rate-config*:

*--latency-limit* _MS_
	Threshold constraint: back off when p99 latency exceeds this value.

*--error-rate-limit* _PERCENT_
	Threshold constraint: back off when error rate exceeds this value.
	Overrides *--ramp-max-errors* in adaptive mode.

*--latency-setpoint* _MS_
	Setpoint (PID) constraint: track p99 at this value with auto-tuned
	gains.

These shortcut flags can be combined. For example,
_--rate-mode adaptive --latency-limit 500 --error-rate-limit 1.0_ composes a
latency ceiling with a hard error cap.

## Full configuration

For more complex arrangements (multiple external signals, custom smoothing,
explicit PID gains), write a JSON config and pass it with *--rate-config*. The
JSON mirrors the *RateConfig::Adaptive* structure in *netanvil-types*.

# CONSTRAINT ARBITRATION

Each tick, the arbiter:

- Constructs an *EvalContext* from the current metrics summary, rate, and
  bounds.
- Calls *Constraint::evaluate(ctx)* on every constraint, collecting
  *ConstraintOutput { intent, severity, class, metadata }*.
- Runs the composition strategy (default: *MinSelector* -- lowest desired
  rate wins).
- Applies policy layers in order: progressive ceiling, known-good floor,
  cooldown, rate-change limits.
- Emits a *RateDecision* carrying the target RPS and the next evaluation
  interval.

Two constraint classes exist:

- *Threshold* (AIMD): extracts a metric, smooths it, compares against a
  threshold, produces graduated hold/backoff actions based on severity. Used
  by *--latency-limit*, *--error-rate-limit*, and external signal limits.
- *Setpoint* (PID): drives a metric toward a target value in log-rate space
  with back-calculation tracking for bumpless transfer when non-binding. Used
  by *--latency-setpoint*.

# EXTERNAL SIGNALS

The controller can consume signals from HTTP response headers as an additional
metric source.

*--response-signal* _SPEC_ (repeatable)
	Extract a numeric value from a response header. Format: _Name_ or
	_Name:aggregation_ where aggregation is _mean_ (default), _max_, or
	_last_. Use with *--pid-metric external:Name* or in a constraint --
	for example, _--response-signal 'X-Load:mean'_ combined with
	_--pid-constraint 'external:X-Load < 75'_.

# CONTROL TRACE

*--control-trace* _FILE_
	Record per-tick controller decisions to a structured JSONL file. Each
	line captures the metrics summary, each constraint's output, the
	composition result, and the final rate decision. Only active in
	*adaptive* mode. Used for post-hoc controller analysis and offline
	replay through alternate configurations. See the analysis scripts in
	_scripts/analyze-trace.py_ and _scripts/trace-report.py_.

# EXAMPLES

The following fragments are flag sets to combine with *netanvil-cli test*(1)
or *netanvil-cli leader*(1) invocations.

Static rate for steady-state throughput:

	netanvil-cli test --rate-mode static --rps 5000 [...]

Step profile -- warm up, spike, recover:

	netanvil-cli test --rate-mode step --steps 0s:500,30s:5000,60s:500 [...]

PID tracking p99 at 200ms with autotuning:

	netanvil-cli test --rate-mode pid --pid-metric latency-p99 --pid-target 200 [...]

Adaptive with latency ceiling and error cap:

	netanvil-cli test --rate-mode adaptive --latency-limit 500 --error-rate-limit 1.0 [...]

Full adaptive config from JSON:

	netanvil-cli test --rate-config configs/saturation-probe.json [...]

# SEE ALSO

*netanvil-cli-test*(1), *netanvil-cli-leader*(1), *netanvil-protocols*(7)
