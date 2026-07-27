package dev.thompgt.transit.stream.domain;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule;
import java.time.Instant;
import java.time.LocalDate;
import org.junit.jupiter.api.Test;

class StopEventTest {

    private static StopEvent event(Integer delaySeconds, boolean cancelled) {
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
                "veh-7");
    }

    @Test
    void reportsADelayMeasurementWhenOneExists() {
        assertTrue(event(120, false).hasDelayMeasurement());
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

    /** Unknown fields must not break the consumer when the producer adds one. */
    @Test
    void deserializesIgnoringUnknownFields() throws Exception {
        var mapper = new ObjectMapper().registerModule(new JavaTimeModule());
        var json = """
                {
                  "eventId": "abc123",
                  "serviceDate": "2026-07-26",
                  "agencyId": "MTA_LIRR",
                  "routeKey": "MTA_LIRR:1",
                  "routeId": "1",
                  "tripId": "GO201_26_2",
                  "stopKey": "MTA_LIRR:102",
                  "stopId": "102",
                  "stopSequence": 1,
                  "directionId": 0,
                  "scheduledArrival": "2026-07-26T05:05:00Z",
                  "actualArrival": "2026-07-26T05:07:00Z",
                  "delaySeconds": 120,
                  "dwellSeconds": 30,
                  "headwaySeconds": 600,
                  "cancelled": false,
                  "vehicleId": "veh-7",
                  "someFieldAddedLater": 42
                }
                """;

        StopEvent parsed = mapper.readValue(json, StopEvent.class);

        assertEquals("MTA_LIRR:1", parsed.routeKey());
        assertEquals(120, parsed.delaySeconds());
        assertEquals(LocalDate.of(2026, 7, 26), parsed.serviceDate());
        assertTrue(parsed.hasDelayMeasurement());
    }
}
