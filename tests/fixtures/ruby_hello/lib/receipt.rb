# frozen_string_literal: true

module Billing
  # Companion to Invoice (billing.rb): same module, separate file.
  class Receipt
    def initialize(amount)
      @amount = amount
    end

    def print
      "receipt: #{@amount}"
    end
  end
end
