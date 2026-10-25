-- Add column "source_kind" to table: "mod_units"
ALTER TABLE `mod_units` ADD COLUMN `source_kind` text NOT NULL DEFAULT 'jar';
-- Add column "entry_kind" to table: "translation_entries"
ALTER TABLE `translation_entries` ADD COLUMN `entry_kind` text NOT NULL DEFAULT 'lang';
-- Add column "document_path" to table: "translation_entries"
ALTER TABLE `translation_entries` ADD COLUMN `document_path` text NULL;
-- Add column "document_pointer" to table: "translation_entries"
ALTER TABLE `translation_entries` ADD COLUMN `document_pointer` text NULL;
-- Drop index "translation_entries_mod_unit_id_namespace_translation_key" from table: "translation_entries"
DROP INDEX `translation_entries_mod_unit_id_namespace_translation_key`;
-- Create index "translation_entries_mod_unit_id_namespace_entry_kind_translation_key" to table: "translation_entries"
CREATE UNIQUE INDEX `translation_entries_mod_unit_id_namespace_entry_kind_translation_key` ON `translation_entries` (`mod_unit_id`, `namespace`, `entry_kind`, `translation_key`);
-- Create "file_deployments" table
CREATE TABLE `file_deployments` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `target_path` text NOT NULL,
  `existed_before` integer NOT NULL,
  `previous_sha256` text NULL,
  `backup_path` text NULL,
  `deployed_sha256` text NOT NULL,
  `deployed_at` text NOT NULL,
  `restored_at` text NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "file_deployments_active_target" to table: "file_deployments"
CREATE UNIQUE INDEX `file_deployments_active_target` ON `file_deployments` (`mod_unit_id`, `target_path`) WHERE restored_at IS NULL;
