package dev.thompgt.transit.stream.config;

import jakarta.validation.Valid;
import jakarta.validation.constraints.NotBlank;
import jakarta.validation.constraints.NotEmpty;
import jakarta.validation.constraints.NotNull;
import jakarta.validation.constraints.Positive;
import java.time.Duration;
import java.util.List;
import org.springframework.boot.context.properties.ConfigurationProperties;
import org.springframework.validation.annotation.Validated;

/**
 * Everything under {@code tde:} in {@code application.yml}, bound and validated.
 *
 * <p>This type exists because the configuration had no type at all. {@code @ConfigurationPropertiesScan}
 * was switched on and the whole {@code tde:} block was written out — topics, window sizes, the
 * on-time threshold, anomaly thresholds, the Parquet output directory and flush cron — with
 * nothing on the classpath binding any of it. Spring does not object to configuration nobody
 * reads, so a typo (<code>threshhold-seconds</code>, <code>stop_events</code>) was undetectable:
 * the application started clean and the setting simply had no effect.
 *
 * <p>Validation is the second half of that. A misspelled key now fails to bind and a nonsensical
 * value — an empty topic name, a zero-second window, a negative observation floor — fails the
 * context at startup rather than at the first message.
 *
 * @param topics Kafka topics this service reads and writes
 * @param windows rolling aggregate windows, keyed by route and by stop
 * @param onTimeThresholdSeconds an arrival within this many seconds of schedule is on time;
 *     configurable because agencies define it differently, commuter rail far more strictly than
 *     subway
 * @param anomaly when a route's current window counts as anomalous
 * @param parquet where completed windows are flushed for the cube
 */
@Validated
@ConfigurationProperties(prefix = "tde")
public record TdeProperties(
        @Valid @NotNull Topics topics,
        @NotEmpty List<@NotNull Duration> windows,
        @Positive int onTimeThresholdSeconds,
        @Valid @NotNull Anomaly anomaly,
        @Valid @NotNull Parquet parquet) {

    /**
     * @param stopEvents one message per vehicle arrival at one stop, partitioned by route so the
     *     per-route windows stay on one consumer instance
     * @param alerts service alerts
     * @param ingestHealth feed staleness and poll outcomes from the ingest
     */
    public record Topics(
            @NotBlank String stopEvents, @NotBlank String alerts, @NotBlank String ingestHealth) {}

    /**
     * @param thresholdSeconds a route is anomalous when its current window mean delay exceeds the
     *     historical baseline for the same day-of-week and hour by this much
     * @param minObservations below this many observations the window is too thin to judge
     */
    public record Anomaly(@Positive int thresholdSeconds, @Positive int minObservations) {}

    /**
     * @param outputDir shared volume the cube reads
     * @param flushCron when compaction flushes completed windows there
     */
    public record Parquet(@NotBlank String outputDir, @NotBlank String flushCron) {}
}
