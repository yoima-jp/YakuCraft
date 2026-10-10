-- Create "jar_replacements" table
CREATE TABLE `jar_replacements` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `target_path` text NOT NULL,
  `backup_path` text NOT NULL,
  `previous_sha256` text NOT NULL,
  `written_sha256` text NOT NULL,
  `created_at` text NOT NULL,
  `restored_at` text NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "jar_replacements_active_target" to table: "jar_replacements"
CREATE UNIQUE INDEX `jar_replacements_active_target` ON `jar_replacements` (`mod_unit_id`, `target_path`) WHERE restored_at IS NULL;
