package dev.thompgt.transit.stream.domain;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.time.LocalDate;
import org.junit.jupiter.api.Test;

class StopEventTest {

    private static final ObjectMapper MAPPER = new ObjectMapper().registerModule(new JavaTimeModule());

    /**
     * The golden wire fixture, shared with the Rust producer. Resolved from the module directory
     * (Surefire's working directory) rather than the classpath, so both languages read the one
     * file and a copy cannot drift from it.
     */
    private static final Path CONTRACT = Path.of("..", "..", "contracts", "stop_event.json");

    private static StopEvent event(Integer delaySeconds, boolean cancelled) {
        return event(delaySeconds, cancelled, ScheduleRelationship.SCHEDULED);
    }

    private static StopEvent event(
            Integer delaySeconds, boolean cancelled, ScheduleRelationship relationship) {
        return new StopEvent(
                "abc123",
                LocalDate.of(2026, 7, 26),
                "MTA_LIRR",
                "MTA_LIRR:1",
                "1",
                "GO201_26_2",
                "MTA_LIRR:102",
                "102",
                1,
                0,
                Instant.parse("2026-07-26T05:05:00Z"),
                cancelled ? null : Instant.parse("2026-07-26T05:07:00Z"),
                delaySeconds,
                30,
                600,
                cancelled,
                relationship,
                "veh-7");
    }

    @Test
    void reportsADelayMeasurementWhenOneExists() {
        assertTrue(event(120, false).hasDelayMeasurement());
    }

    /**
     * A delay is a difference against a scheduled time, so it only means something where one
     * exists. An ADDED trip has no static schedule at all and a SKIPPED stop was never called at,
     * yet both can arrive carrying a delaySeconds — and a boolean `cancelled` waves both through.
     */
    @Test
    void onlyAScheduledRelationshipYieldsAMeasurement() {
        for (ScheduleRelationship relationship : ScheduleRelationship.values()) {
            boolean usable = event(120, false, relationship).hasDelayMeasurement();
            assertEquals(
                    relationship == ScheduleRelationship.SCHEDULED,
                    usable,
                    relationship + " must " + (usable ? "not " : "") + "yield a measurement");
        }
    }

    /** Silence is not consent: an unstated relationship is not SCHEDULED. */
    @Test
    void anUnstatedRelationshipYieldsNoMeasurement() {
        assertFalse(event(120, false, null).hasDelayMeasurement());
    }

    /** Counting a cancellation as zero delay makes a failing route look punctual. */
    @Test
    void cancelledTripsCarryNoDelayMeasurement() {
        assertFalse(event(null, true).hasDelayMeasurement());
    }

    /** A cancelled trip that somehow still carries a delay is still excluded. */
    @Test
    void cancellationWinsOverAPresentDelayValue() {
        assertFalse(event(0, true).hasDelayMeasurement());
    }

    @Test
    void eventsAwaitingAnActualArrivalAreExcluded() {
        assertFalse(event(null, false).hasDelayMeasurement());
    }

    /**
     * The checked-in wire contract must bind every component.
     *
     * <p>This is the test that would have caught the vocabulary split: the record's components are
     * camelCase, the Parquet columns the producer is named after are snake_case, and nothing
     * pinned which went on the wire. A producer following the Parquet names against a
     * camelCase-only consumer parses into nulls and zeroes without an error anywhere, so every
     * field is asserted here rather than a representative few.
     */
    @Test
    void parsesTheCheckedInWireContract() throws Exception {
        StopEvent parsed = MAPPER.readValue(CONTRACT.toFile(), StopEvent.class);

        assertEquals("9f2c1ab4e7d05386", parsed.eventId());
        assertEquals(LocalDate.of(2026, 7, 26), parsed.serviceDate());
        assertEquals("MTA_LIRR", parsed.agencyId());
        assertEquals("MTA_LIRR:1", parsed.routeKey());
        assertEquals("1", parsed.routeId());
        assertEquals("GO201_26_2", parsed.tripId());
        assertEquals("MTA_LIRR:102", parsed.stopKey());
        assertEquals("102", parsed.stopId());
        assertEquals(1, parsed.stopSequence());
        assertEquals(Integer.valueOf(0), parsed.directionId());
        assertEquals(Instant.parse("2026-07-26T05:05:00Z"), parsed.scheduledArrival());
        assertEquals(Instant.parse("2026-07-26T05:07:00Z"), parsed.actualArrival());
        assertEquals(Integer.valueOf(120), parsed.delaySeconds());
        assertEquals(Integer.valueOf(30), parsed.dwellSeconds());
        assertEquals(Integer.valueOf(600), parsed.headwaySeconds());
        assertFalse(parsed.cancelled());
        assertEquals(ScheduleRelationship.SCHEDULED, parsed.scheduleRelationship());
        assertEquals("veh-7", parsed.vehicleId());
        assertTrue(parsed.hasDelayMeasurement());
    }

    /** camelCase is not the wire form; a producer sending it must not bind. */
    @Test
    void rejectsTheCamelCaseSpellingOfTheWireForm() throws Exception {
        var json = """
                {"event_id": "abc123", "route_key": "MTA_LIRR:1", "routeKey": null, "delaySeconds": 9}
                """;

        StopEvent parsed = MAPPER.readValue(json, StopEvent.class);

        // route_key bound; the camelCase routeKey alongside it was ignored as unknown rather than
        // overwriting it, and camelCase delaySeconds did not bind at all.
        assertEquals("MTA_LIRR:1", parsed.routeKey());
        assertNull(parsed.delaySeconds());
    }

    /** Unknown fields must not break the consumer when the producer adds one. */
    @Test
    void deserializesIgnoringUnknownFields() throws Exception {
        var json = """
                {
                  "event_id": "abc123",
                  "service_date": "2026-07-26",
                  "delay_seconds": 120,
                  "is_cancelled": false,
                  "schedule_relationship": "SCHEDULED",
                  "some_field_added_later": 42
                }
                """;

        StopEvent parsed = MAPPER.readValue(json, StopEvent.class);

        assertEquals("abc123", parsed.eventId());
        assertEquals(Integer.valueOf(120), parsed.delaySeconds());
        assertEquals(LocalDate.of(2026, 7, 26), parsed.serviceDate());
        assertTrue(parsed.hasDelayMeasurement());
    }

    /**
     * GTFS makes {@code direction_id} optional and some LIRR trips leave it blank, which the
     * ingest preserves as a null Parquet value. A primitive here would let Jackson map both an
     * explicit null and an omitted field to 0 — a real direction — silently inflating one side of
     * every directional comparison.
     */
    @Test
    void anUnstatedDirectionStaysNullRatherThanBecomingZero() throws Exception {
        var explicitNull = """
                {"event_id": "abc123", "direction_id": null}
                """;
        var omitted = """
                {"event_id": "abc123"}
                """;

        assertNull(MAPPER.readValue(explicitNull, StopEvent.class).directionId());
        assertNull(MAPPER.readValue(omitted, StopEvent.class).directionId());
    }

    @Test
    void aStatedDirectionSurvives() throws Exception {
        var json = """
                {"event_id": "abc123", "direction_id": 1}
                """;

        assertEquals(Integer.valueOf(1), MAPPER.readValue(json, StopEvent.class).directionId());
    }

    /** The fixture must actually be where both toolchains look for it. */
    @Test
    void theWireContractIsCheckedIn() {
        assertTrue(Files.isRegularFile(CONTRACT), CONTRACT.toAbsolutePath() + " is missing");
    }
}
