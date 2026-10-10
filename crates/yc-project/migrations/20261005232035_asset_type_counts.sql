-- Create "asset_type_counts" table
CREATE TABLE `asset_type_counts` (
  `mod_unit_id` integer NOT NULL,
  `asset_type` text NOT NULL,
  `asset_count` integer NOT NULL,
  PRIMARY KEY (`mod_unit_id`, `asset_type`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
