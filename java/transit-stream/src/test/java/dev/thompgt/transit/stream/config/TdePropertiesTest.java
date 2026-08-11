package dev.thompgt.transit.stream.config;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import java.time.Duration;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.autoconfigure.context.ConfigurationPropertiesAutoConfiguration;
import org.springframework.boot.autoconfigure.validation.ValidationAutoConfiguration;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.boot.test.context.ConfigDataApplicationContextInitializer;
import org.springframework.boot.test.context.runner.ApplicationContextRunner;

/**
 * That the checked-in {@code application.yml} binds, key for key.
 *
 * <p>Without this the whole {@code tde:} block was inert. A typo in it — a doubled letter in
 * {@code threshold}, an underscore where a hyphen belongs — produced no error anywhere, because
 * nothing was reading the block at all; the application started clean and the setting quietly did
 * nothing. Asserting the *values* rather than merely that a bean exists is the point: a record
 * bound with every field null would otherwise pass.
 */
class TdePropertiesTest {

    @EnableConfigurationProperties(TdeProperties.class)
    static class Enable {}

    /** The real file, loaded the way the application loads it. */
    private final ApplicationContextRunner runner = new ApplicationContextRunner()
            .withInitializer(new ConfigDataApplicationContextInitializer())
            .withConfiguration(AutoConfigurations.of(
                    ConfigurationPropertiesAutoConfiguration.class, ValidationAutoConfiguration.class))
            .withUserConfiguration(Enable.class);

    @Test
    void theCheckedInConfigurationBinds() {
        runner.run(context -> {
            TdeProperties properties = context.getBean(TdeProperties.class);

            assertEquals("transit.stop_events", properties.topics().stopEvents());
            assertEquals("transit.alerts", properties.topics().alerts());
            assertEquals("transit.ingest_health", properties.topics().ingestHealth());

            assertEquals(
                    List.of(Duration.ofMinutes(5), Duration.ofHours(1), Duration.ofHours(24)),
                    properties.windows());

            assertEquals(300, properties.onTimeThresholdSeconds());
            assertEquals(120, properties.anomaly().thresholdSeconds());
            assertEquals(20, properties.anomaly().minObservations());

            assertEquals("/data/parquet", properties.parquet().outputDir());
            assertEquals("0 */5 * * * *", properties.parquet().flushCron());
        });
    }

    /** A value that cannot be true must fail the context, not the first message. */
    @Test
    void anImpossibleObservationFloorFailsAtStartup() {
        runner.withPropertyValues("tde.anomaly.min-observations=0")
                .run(context -> assertNotNull(
                        context.getStartupFailure(), "a zero observation floor must not bind"));
    }

    /** An empty topic name is a configuration mistake, not an empty topic. */
    @Test
    void aBlankTopicFailsAtStartup() {
        runner.withPropertyValues("tde.topics.stop-events=")
                .run(context ->
                        assertNotNull(context.getStartupFailure(), "a blank topic must not bind"));
    }
}
