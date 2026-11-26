-- Boxes created through the resumable upload API stay hidden while their files arrive.
-- A non-null value marks such a pending box and holds the lifetime it gets once complete.
ALTER TABLE boxes ADD COLUMN pending_lifetime INTEGER;
