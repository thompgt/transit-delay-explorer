package dev.thompgt.transit.stream.domain;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.PropertyNamingStrategies;
import com.fasterxml.jackson.databind.annotation.JsonNaming;
import java.time.Instant;
import java.time.LocalDate;

/**
 * One vehicle arrival at one stop — the unit the Rust ingest publishes and
 * everything downstream aggregates over.
 *
 * <p>The wire form is <strong>snake_case</strong>, matching the Parquet column
 * vocabulary rather than the Java component names, so one name means one thing
 * across the project. {@code contracts/stop_event.json} is the golden fixture
 * both sides are tested against; see {@code contracts/README.md}.
 *
 * <p>{@code routeKey} and {@code stopKey} are the namespaced surrogates
 * ({@code MTA_LIRR:1}), not the raw GTFS ids. Route ids collide across the
 * three MTA agencies, so aggregating on the bare id would silently merge three
 * unrelated railroads into one line.
 *
 * <p>Nullable by design: {@code actualArrival} and {@code delaySeconds} are
 * absent for a cancelled trip, {@code headwaySeconds} is absent for the
 * first vehicle of the day on a route/stop/direction, and {@code directionId}
 * is absent wherever the feed declined to state one. GTFS makes
 * {@code direction_id} optional and some LIRR trips leave it blank, so the
 * ingest writes it as a nullable Parquet column rather than defaulting it —
 * {@code 0} is a real direction, and a primitive here would let Jackson turn
 * every unstated direction into direction 0, inflating one side of every
 * directional comparison.
 *
 * @param eventId        hash of agency + trip + stop + service date
 * @param serviceDate    agency-local service date, the Parquet partition key
 * @param agencyId       owning agency
 * @param routeKey       {@code {agencyId}:{routeId}}
 * @param routeId        raw GTFS route id, for display
 * @param tripId         raw GTFS trip id
 * @param stopKey        {@code {agencyId}:{stopId}}
 * @param stopId         raw GTFS stop id
 * @param stopSequence   position along the trip
 * @param directionId    0 or 1, or null where the feed does not state one
 * @param scheduledArrival resolved from GTFS, midnight-rollover aware
 * @param actualArrival  null when the trip was cancelled
 * @param delaySeconds   negative means early; null when cancelled
 * @param dwellSeconds   departure minus arrival
 * @param headwaySeconds gap since the previous vehicle on this route/stop/direction
 * @param cancelled      whether the realtime feed cancelled this trip
 * @param vehicleId      nullable — not every feed reports one
 */
@JsonIgnoreProperties(ignoreUnknown = true)
@JsonNaming(PropertyNamingStrategies.SnakeCaseStrategy.class)
public record StopEvent(
        String eventId,
        LocalDate serviceDate,
        String agencyId,
        String routeKey,
        String routeId,
        String tripId,
        String stopKey,
        String stopId,
        int stopSequence,
        Integer directionId,
        Instant scheduledArrival,
        Instant actualArrival,
        Integer delaySeconds,
        Integer dwellSeconds,
        Integer headwaySeconds,
        // The one component whose snake_case form is not just its own name: the
        // Parquet column is is_cancelled, and the wire follows the Parquet
        // vocabulary rather than the Java one.
        @JsonProperty("is_cancelled") boolean cancelled,
        String vehicleId) {

    /**
     * Whether this event carries a usable delay measurement. Cancelled trips
     * and events still awaiting an actual arrival must not be folded into
     * delay aggregates — counting a cancellation as zero delay is the single
     * easiest way to make a struggling route look punctual.
     */
    public boolean hasDelayMeasurement() {
        return !cancelled && delaySeconds != null;
    }
}
