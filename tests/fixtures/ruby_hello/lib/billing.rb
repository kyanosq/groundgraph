# frozen_string_literal: true

module Billing
  # An invoice totals an amount plus tax. `total` calls `tax` in the same
  # class (a same-file Calls edge), and `receipt` references `Receipt`
  # defined in receipt.rb (a cross-file reference within the same module).
  class Invoice
    def initialize(amount)
      @amount = amount
    end

    def total
      self.tax + @amount
    end

    def tax
      (@amount * 0.1).to_i
    end

    def receipt
      Receipt.new(total)
    end
  end
end
