-- Create "glossary_terms" table
CREATE TABLE `glossary_terms` (
  `id` integer NULL PRIMARY KEY AUTOINCREMENT,
  `source_term` text NOT NULL,
  `target_term` text NOT NULL,
  `note` text NULL,
  `case_sensitive` integer NOT NULL DEFAULT 0,
  `created_at` text NOT NULL,
  `updated_at` text NOT NULL,
  CHECK (case_sensitive IN (0, 1))
);
-- Create index "glossary_terms_source_term" to table: "glossary_terms"
CREATE UNIQUE INDEX `glossary_terms_source_term` ON `glossary_terms` (`source_term`);
-- Create index "glossary_terms_source_insensitive" to table: "glossary_terms"
CREATE UNIQUE INDEX `glossary_terms_source_insensitive` ON `glossary_terms` ((lower(source_term))) WHERE case_sensitive = 0;
