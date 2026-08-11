package dev.thompgt.transit.stream.domain;

/**
 * How a realtime update relates to the static schedule, as GTFS-RT defines it.
 *
 * <p>This exists because a boolean {@code cancelled} cannot express it. GTFS-RT has five
 * non-scheduled states and they do not mean the same thing: a {@code SKIPPED} stop was never
 * called at, a {@code CANCELED} trip did not run, an {@code ADDED} trip has no static schedule to
 * be measured against at all, {@code DUPLICATED} is a copy of one that does, and {@code NO_DATA}
 * is the feed saying it does not know. Collapsing them into "cancelled or not" makes an ADDED
 * trip — a trip whose scheduled arrival is a fiction — indistinguishable from an ordinary
 * punctual one, and it would be folded straight into the delay aggregates as if it were.
 *
 * <p>Only {@link #SCHEDULED} yields a usable delay measurement; see
 * {@link StopEvent#hasDelayMeasurement()}.
 *
 * <p>Spelling follows the GTFS-RT spec, including its single-L {@code CANCELED}, so a value read
 * off the wire needs no translation table to be recognised.
 */
public enum ScheduleRelationship {

    /** Running as scheduled. The only state a delay can be measured in. */
    SCHEDULED,

    /** Running, but with no static schedule — nothing to be early or late against. */
    ADDED,

    /** Running, and the schedule has no fixed times for it (frequency-based service). */
    UNSCHEDULED,

    /** The trip did not run. */
    CANCELED,

    /** A copy of another trip, running at a different time. */
    DUPLICATED,

    /** The trip was removed from the feed entirely. */
    DELETED,

    /** This stop is not served by an otherwise running trip. */
    SKIPPED,

    /** The feed has no realtime information for this stop. */
    NO_DATA
}
