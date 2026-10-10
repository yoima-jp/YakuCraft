-- Create "project_meta" table
CREATE TABLE `project_meta` (
  `id` text NULL,
  `schema_version` integer NOT NULL,
  `created_at` text NOT NULL,
  `updated_at` text NOT NULL,
  `display_name` text NOT NULL,
  `instance_path` text NULL,
  `target_locale` text NOT NULL DEFAULT 'ja_jp',
  `setup_completed` integer NOT NULL DEFAULT 0,
  PRIMARY KEY (`id`)
);
-- Create "mod_units" table
CREATE TABLE `mod_units` (
  `id` integer NULL,
  `sort_index` integer NOT NULL,
  `source_filename` text NOT NULL,
  `source_sha256` text NOT NULL,
  `stored_path` text NOT NULL,
  `original_path` text NOT NULL,
  `mod_id` text NULL,
  `name` text NULL,
  `version` text NULL,
  `loader` text NULL,
  `minecraft_version` text NULL,
  `review_settings_json` text NOT NULL DEFAULT '{}',
  `created_at` text NOT NULL,
  PRIMARY KEY (`id`)
);
-- Create "mods" table
CREATE TABLE `mods` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `mod_id` text NULL,
  `name` text NULL,
  `version` text NULL,
  `loader` text NULL,
  `minecraft_version` text NULL,
  `metadata_source` text NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create "namespaces" table
CREATE TABLE `namespaces` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `namespace` text NOT NULL,
  `container_path` text NOT NULL DEFAULT '',
  `included` integer NOT NULL DEFAULT 1,
  `entry_count` integer NOT NULL DEFAULT 0,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "namespaces_mod_unit_id_namespace" to table: "namespaces"
CREATE UNIQUE INDEX `namespaces_mod_unit_id_namespace` ON `namespaces` (`mod_unit_id`, `namespace`);
-- Create "nested_archives" table
CREATE TABLE `nested_archives` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `archive_path` text NOT NULL,
  `mod_id` text NULL,
  `name` text NULL,
  `version` text NULL,
  `has_languages` integer NOT NULL DEFAULT 0,
  `skipped_reason` text NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "nested_archives_mod_unit_id_archive_path" to table: "nested_archives"
CREATE UNIQUE INDEX `nested_archives_mod_unit_id_archive_path` ON `nested_archives` (`mod_unit_id`, `archive_path`);
-- Create "languages" table
CREATE TABLE `languages` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `namespace` text NOT NULL,
  `locale` text NOT NULL,
  `format` text NOT NULL,
  `container_path` text NOT NULL DEFAULT '',
  `archive_path` text NOT NULL,
  `entry_count` integer NOT NULL,
  `is_target` integer NOT NULL DEFAULT 0,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "languages_mod_unit_id_namespace_locale_container_path_archive_path" to table: "languages"
CREATE UNIQUE INDEX `languages_mod_unit_id_namespace_locale_container_path_archive_path` ON `languages` (`mod_unit_id`, `namespace`, `locale`, `container_path`, `archive_path`);
-- Create "translation_entries" table
CREATE TABLE `translation_entries` (
  `id` integer NULL PRIMARY KEY AUTOINCREMENT,
  `mod_unit_id` integer NOT NULL,
  `namespace` text NOT NULL,
  `translation_key` text NOT NULL,
  `source_text` text NULL,
  `source_locale` text NULL,
  `target_text` text NULL,
  `target_original_text` text NULL,
  `translated_source_text` text NULL,
  `target_origin` text NOT NULL,
  `sort_index` integer NOT NULL,
  `asset_kind` text NULL,
  `asset_id` text NULL,
  `asset_confidence` text NULL,
  `updated_at` text NOT NULL,
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "translation_entries_mod_unit_id_namespace_translation_key" to table: "translation_entries"
CREATE UNIQUE INDEX `translation_entries_mod_unit_id_namespace_translation_key` ON `translation_entries` (`mod_unit_id`, `namespace`, `translation_key`);
-- Create "entry_flags" table
CREATE TABLE `entry_flags` (
  `entry_id` integer NOT NULL,
  `flag` text NOT NULL,
  PRIMARY KEY (`entry_id`, `flag`),
  CONSTRAINT `0` FOREIGN KEY (`entry_id`) REFERENCES `translation_entries` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create "assets" table
CREATE TABLE `assets` (
  `id` integer NULL,
  `mod_unit_id` integer NOT NULL,
  `namespace` text NOT NULL,
  `asset_type` text NOT NULL,
  `resource_id` text NOT NULL,
  `archive_path` text NOT NULL,
  `metadata_json` text NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `0` FOREIGN KEY (`mod_unit_id`) REFERENCES `mod_units` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create index "assets_mod_unit_id_archive_path" to table: "assets"
CREATE UNIQUE INDEX `assets_mod_unit_id_archive_path` ON `assets` (`mod_unit_id`, `archive_path`);
-- Create "asset_links" table
CREATE TABLE `asset_links` (
  `entry_id` integer NOT NULL,
  `asset_id` integer NOT NULL,
  `relation` text NOT NULL,
  `confidence` text NOT NULL,
  PRIMARY KEY (`entry_id`, `asset_id`, `relation`),
  CONSTRAINT `0` FOREIGN KEY (`asset_id`) REFERENCES `assets` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE,
  CONSTRAINT `1` FOREIGN KEY (`entry_id`) REFERENCES `translation_entries` (`id`) ON UPDATE NO ACTION ON DELETE CASCADE
);
-- Create "ai_jobs" table
CREATE TABLE `ai_jobs` (
  `id` text NULL,
  `provider` text NOT NULL,
  `model` text NULL,
  `status` text NOT NULL,
  `total_items` integer NOT NULL,
  `completed_items` integer NOT NULL DEFAULT 0,
  `created_at` text NOT NULL,
  `finished_at` text NULL,
  `error_message` text NULL,
  PRIMARY KEY (`id`)
);
