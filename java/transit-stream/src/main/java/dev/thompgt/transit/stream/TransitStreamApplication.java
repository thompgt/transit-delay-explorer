package dev.thompgt.transit.stream;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.boot.context.properties.ConfigurationPropertiesScan;
import org.springframework.scheduling.annotation.EnableScheduling;

/**
 * Real-time streaming layer: consumes stop events off Kafka, maintains rolling
 * windowed aggregates, detects anomalies against historical baselines, and
 * serves both a REST API and a live SSE stream.
 *
 * <p>Scheduling is enabled here for the compaction job that flushes completed
 * windows to Parquet for the cube to pick up.
 */
@SpringBootApplication
@ConfigurationPropertiesScan
@EnableScheduling
public class TransitStreamApplication {

    public static void main(String[] args) {
        SpringApplication.run(TransitStreamApplication.class, args);
    }
}
