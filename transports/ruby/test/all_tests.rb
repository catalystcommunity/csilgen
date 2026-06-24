# frozen_string_literal: true

# Aggregator the test runner invokes: it loads the library and every *_test.rb in one
# process so minitest/autorun runs the whole suite without rake or Bundler.
$LOAD_PATH.unshift File.expand_path("../lib", __dir__)

require_relative "test_helper"

Dir.glob(File.join(__dir__, "*_test.rb")).sort.each { |f| require f }
